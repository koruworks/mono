// SPDX-License-Identifier: MPL-2.0
//! A Bot's face (ai.db 24): picked, changed and cleared through the Bot's
//! ordinary `Update`, replayed exactly, and refused outside the face table.
use super::*;
use koru_ai_core::bots::{BOT_AVATAR_COUNT, DEFAULT_AVATAR};

fn update(
    repo: &mut Repository,
    authority: &Arc<Authority>,
    command: BotCommand,
) -> ApiResult<Bot> {
    execute_bot_command(repo, authority.clone(), |_| Ok(()), context(true), command).map(
        |response| match response {
            BotResponse::Bot { bot } => bot,
            other => panic!("unexpected {other:?}"),
        },
    )
}

fn read(repo: &mut Repository, authority: &Arc<Authority>, bot: BotId) -> Bot {
    update(repo, authority, BotCommand::Read { bot_id: bot }).unwrap()
}

#[test]
fn an_update_picks_changes_and_clears_a_face_and_replays_exactly() {
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    // Made with no face: none stored, and it wears the default one.
    assert_eq!(bot.config.avatar, None);
    assert_eq!(bot.avatar(), DEFAULT_AVATAR);

    // Pick one. Only the face moves; the revision moves once.
    let mut config = bot.config.clone();
    config.avatar = Some(7);
    let pick = BotCommand::Update {
        request_id: BotRequestId::mint(),
        bot_id: bot.id,
        expected_revision: bot.revision,
        config: config.clone(),
    };
    let picked = update(&mut repo, &authority, pick.clone()).unwrap();
    assert_eq!(picked.config, config);
    assert_eq!(picked.avatar(), 7);
    assert_eq!(picked.revision, bot.revision + 1);
    assert_eq!(picked.policy_revision, bot.policy_revision);
    assert_eq!(read(&mut repo, &authority, bot.id), picked);
    // The same request again is the same answer, and nothing moves.
    assert_eq!(update(&mut repo, &authority, pick.clone()).unwrap(), picked);
    assert_eq!(
        read(&mut repo, &authority, bot.id).revision,
        picked.revision
    );
    // The same request naming another face is a different request.
    let BotCommand::Update {
        request_id,
        bot_id,
        expected_revision,
        config: mut other,
    } = pick
    else {
        unreachable!()
    };
    other.avatar = Some(8);
    assert_eq!(
        update(
            &mut repo,
            &authority,
            BotCommand::Update {
                request_id,
                bot_id,
                expected_revision,
                config: other,
            },
        )
        .unwrap_err()
        .code,
        "request_conflict"
    );

    // A save that carries the face over (as the settings do, from the Bot
    // they hold) keeps it.
    let mut renamed = picked.config.clone();
    renamed.name = "Scout".into();
    let kept = update(
        &mut repo,
        &authority,
        BotCommand::Update {
            request_id: BotRequestId::mint(),
            bot_id: bot.id,
            expected_revision: picked.revision,
            config: renamed,
        },
    )
    .unwrap();
    assert_eq!(kept.config.avatar, Some(7));
    assert_eq!(kept.config.name, "Scout");

    // Outside the table: refused, and nothing changes.
    let mut outside = kept.config.clone();
    outside.avatar = Some(BOT_AVATAR_COUNT);
    assert_eq!(
        update(
            &mut repo,
            &authority,
            BotCommand::Update {
                request_id: BotRequestId::mint(),
                bot_id: bot.id,
                expected_revision: kept.revision,
                config: outside,
            },
        )
        .unwrap_err()
        .code,
        "invalid_request"
    );
    assert_eq!(read(&mut repo, &authority, bot.id), kept);

    // Cleared: the default face again.
    let mut cleared = kept.config.clone();
    cleared.avatar = None;
    let plain = update(
        &mut repo,
        &authority,
        BotCommand::Update {
            request_id: BotRequestId::mint(),
            bot_id: bot.id,
            expected_revision: kept.revision,
            config: cleared,
        },
    )
    .unwrap();
    assert_eq!(plain.config.avatar, None);
    assert_eq!(plain.avatar(), DEFAULT_AVATAR);
    // Listed with the workspace's Bots as it reads.
    let Ok(BotResponse::List { bots }) = execute_bot_command(
        &mut repo,
        authority.clone(),
        |_| Ok(()),
        context(true),
        BotCommand::List {
            workspace: authority.workspace,
        },
    ) else {
        panic!("the list")
    };
    assert_eq!(bots, vec![plain]);
}

#[test]
fn a_legacy_create_carries_a_face_and_refuses_one_outside_the_table() {
    let (mut repo, authority, command) = fixture();
    let BotCommand::Create {
        home_workspace,
        mut config,
        access,
        selected_pages,
        ..
    } = command
    else {
        unreachable!()
    };
    config.avatar = Some(BOT_AVATAR_COUNT);
    let create = |config: BotConfig| BotCommand::Create {
        request_id: BotRequestId::mint(),
        bot_id: BotId::mint(),
        home_workspace,
        config,
        access,
        selected_pages: selected_pages.clone(),
    };
    assert_eq!(
        update(&mut repo, &authority, create(config.clone()))
            .unwrap_err()
            .code,
        "invalid_request"
    );
    config.avatar = Some(23);
    let made = update(&mut repo, &authority, create(config)).unwrap();
    assert_eq!(made.config.avatar, Some(23));
    assert_eq!(read(&mut repo, &authority, made.id).avatar(), 23);
}
