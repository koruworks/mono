// SPDX-License-Identifier: MPL-2.0
//! Cohort `m8_team_refuses_submission`: the two admission reads under a team
//! pause (contract 3.2 and 3.3, rows 1 to 5), on an in-memory ai.db.
use super::*;

const PAUSE: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

fn receipts(repo: &Repository) -> i64 {
    repo.connection
        .query_row(
            "SELECT count(*) FROM bot_command_receipts WHERE command_kind IN ('start','continue')",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

fn bot_rows(repo: &Repository) -> Vec<String> {
    let mut statement = repo
        .connection
        .prepare("SELECT bot_id||'/'||revision||'/'||policy_revision||'/'||paused||'/'||updated_at_ms FROM bots ORDER BY bot_id")
        .unwrap();
    statement
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn pause_team(repo: &mut Repository, owner: BotOwner, expected: u64, paused: bool) {
    let change = repo
        .change_team_pause(owner, BotRequestId::mint(), PAUSE, expected, paused, 20)
        .unwrap();
    assert!(change.changed().is_some());
}

#[test]
fn m8_team_refuses_submission_from_the_commit_of_the_row_and_writes_no_receipt() {
    let (mut repo, bot, owner, first) = fixture();
    assert!(repo.bot_admitting(bot.id).unwrap());
    let bots_before = bot_rows(&repo);

    pause_team(&mut repo, owner, 0, true);
    // The row is still `pending`: the AI side refuses already.
    assert_eq!(
        repo.team_pause(owner).unwrap().unwrap().stage,
        TeamPauseStage::Pending
    );
    assert_eq!(
        repo.prepare_bot_submission(&first, HASH, 1, 21),
        Err(BotError::TeamPaused)
    );
    assert_eq!(BotError::TeamPaused.code(), "team_paused");
    assert_eq!(receipts(&repo), 0, "no receipt was written");
    // Recovery of a pending submission: not submitted while held.
    assert!(!repo.bot_admitting(bot.id).unwrap());
    // No Bot row changed: no revision moved and no Bot was marked paused.
    assert_eq!(bot_rows(&repo), bots_before);
    assert!(!repo.bot(bot.id).unwrap().unwrap().paused);

    // After the resume the same submission is prepared.
    pause_team(&mut repo, owner, 1, false);
    assert!(repo.bot_admitting(bot.id).unwrap());
    repo.prepare_bot_submission(&first, HASH, 1, 30).unwrap();
    assert_eq!(receipts(&repo), 1);
    assert_eq!(bot_rows(&repo), bots_before);
}

#[test]
fn m8_team_refuses_submission_but_answers_the_exact_retry_of_one_prepared_before() {
    let (mut repo, bot, owner, first) = fixture();
    let prepared = repo.prepare_bot_submission(&first, HASH, 1, 10).unwrap();
    pause_team(&mut repo, owner, 0, true);
    // A submission prepared before the pause committed keeps its receipt;
    // its exact retry is answered from it. It is not offered to its owner
    // while the team is paused.
    assert_eq!(
        repo.prepare_bot_submission(&first, HASH, 1, 21).unwrap(),
        prepared
    );
    assert!(!repo.bot_admitting(bot.id).unwrap());
    // A new one is refused.
    let second = intent(&bot);
    assert_eq!(
        repo.prepare_bot_submission(&second, HASH, 1, 22),
        Err(BotError::TeamPaused)
    );
    assert_eq!(receipts(&repo), 1);
}

#[test]
fn m8_team_refuses_submission_of_a_bot_created_while_paused_and_accepts_a_bots_own_resume() {
    let (mut repo, bot, owner, _) = fixture();
    pause_team(&mut repo, owner, 0, true);

    // A Bot created while the team is paused is created as today, and is
    // refused its first task: the team value is per owner.
    let mut created = super::super::super::tests::fixture_bot();
    created.user_scope = bot.user_scope;
    created.owner_device = bot.owner_device;
    created.home_workspace = bot.home_workspace;
    created.config.role.workspace = bot.home_workspace;
    created.config.working_folder = if cfg!(windows) {
        "D:/disposable-two"
    } else {
        "/disposable-two"
    }
    .into();
    repo.save_bot(
        &created,
        ProfileId::new(),
        BotRequestId::mint(),
        "create",
        None,
    )
    .unwrap();
    assert!(!repo.bot_admitting(created.id).unwrap());
    assert_eq!(
        repo.prepare_bot_submission(&intent(&created), HASH, 1, 23),
        Err(BotError::TeamPaused)
    );

    // A Bot's own Pause and Resume are independent of the team: both are
    // accepted, and the Bot stays held while either is paused.
    let pause = repo
        .prepare_bot_pause(owner, bot.id, BotRequestId::mint(), 1, true, HASH, 24)
        .unwrap();
    repo.acknowledge_bot_pause(
        owner,
        bot.id,
        pause.intent.barrier.request,
        &pause.intent.barrier,
        25,
    )
    .unwrap();
    let resume = repo
        .prepare_bot_pause(owner, bot.id, BotRequestId::mint(), 2, false, HASH, 26)
        .unwrap();
    repo.acknowledge_bot_pause(
        owner,
        bot.id,
        resume.intent.barrier.request,
        &resume.intent.barrier,
        27,
    )
    .unwrap();
    let current = repo.bot(bot.id).unwrap().unwrap();
    assert!(!current.paused, "the Bot's own Resume was accepted");
    assert!(!repo.bot_admitting(bot.id).unwrap(), "held by the team");
    assert_eq!(
        repo.prepare_bot_submission(&intent(&current), HASH, current.revision, 28),
        Err(BotError::TeamPaused)
    );

    // Paused both ways, the Bot's own pause is named first.
    let again = repo
        .prepare_bot_pause(owner, bot.id, BotRequestId::mint(), 3, true, HASH, 29)
        .unwrap();
    repo.acknowledge_bot_pause(
        owner,
        bot.id,
        again.intent.barrier.request,
        &again.intent.barrier,
        30,
    )
    .unwrap();
    let current = repo.bot(bot.id).unwrap().unwrap();
    assert_eq!(
        repo.prepare_bot_submission(&intent(&current), HASH, current.revision, 31),
        Err(BotError::Forbidden)
    );
    // The team's resume leaves a Bot paused on its own paused.
    pause_team(&mut repo, owner, 1, false);
    assert!(!repo.bot_admitting(bot.id).unwrap());
    assert!(repo.bot_admitting(created.id).unwrap());
}

/// Another person's team, and the same person's team on another computer,
/// hold nothing here.
#[test]
fn m8_team_refuses_submission_only_for_the_paused_owner() {
    let (mut repo, bot, owner, first) = fixture();
    let other_person = BotOwner {
        user_scope: UserScopeId::new(),
        device: owner.device,
    };
    let other_computer = BotOwner {
        user_scope: owner.user_scope,
        device: DeviceId::from_uuid(Uuid::new_v4()),
    };
    for other in [other_person, other_computer] {
        pause_team(&mut repo, other, 0, true);
    }
    assert!(repo.bot_admitting(bot.id).unwrap());
    repo.prepare_bot_submission(&first, HASH, 1, 40).unwrap();
}
