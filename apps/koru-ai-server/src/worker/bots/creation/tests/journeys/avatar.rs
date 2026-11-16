// SPDX-License-Identifier: MPL-2.0
//! A creation's picked face (ai.db 24) through the real writer: carried to
//! the Bot, replayed exactly with and without one, across a restart, and a
//! conflict when the same request names another.
use super::*;
use koru_ai_core::bots::{BOT_AVATAR_COUNT, derived_avatar};

/// `command` with `avatar` in place of the face it named.
fn with_avatar(command: &BotCommand, avatar: Option<u8>) -> BotCommand {
    let BotCommand::CreateBotWithRole {
        request_id,
        bot_id,
        home_workspace,
        role,
        folder,
        config,
        access,
        selected_pages,
    } = command.clone()
    else {
        unreachable!()
    };
    BotCommand::CreateBotWithRole {
        request_id,
        bot_id,
        home_workspace,
        role,
        folder,
        config: BotCreationConfig { avatar, ..config },
        access,
        selected_pages,
    }
}

#[test]
fn a_creation_with_a_face_makes_a_bot_wearing_it_and_replays_exactly() {
    let owner = FakeOwner::new();
    let fs = FakeFs::with(&[root()]);
    let h = start(Repository::in_memory().unwrap(), &owner, &fs);
    let plain = create(&owner, authored(), BotFolderChoice::Managed {});
    let command = with_avatar(&plain, Some(11));
    let (request, bot_id) = request_of(&command);
    assert_eq!(
        h.creation(command.clone()).unwrap().stage,
        BotCreationStage::Pending
    );
    // While it moves, the same request with another face, or with none, is
    // a different request: nothing about the creation changes.
    for other in [Some(12), None] {
        assert_eq!(
            h.creation(with_avatar(&command, other)).unwrap_err().code,
            "request_conflict"
        );
    }
    let done = h.settle(request);
    assert_eq!(done.stage, BotCreationStage::Completed, "{done:?}");
    let bot = done.bot.clone().expect("the completed Bot");
    assert_eq!(bot.id, bot_id);
    assert_eq!(bot.config.avatar, Some(11));
    assert_eq!(bot.avatar(), 11);
    // An exact replay returns the completed Bot and does nothing again.
    let (calls, creates) = (owner.store_calls(), fs.creates().len());
    let again = h.creation(command.clone()).unwrap();
    assert_eq!(again.stage, BotCreationStage::Completed);
    assert_eq!(again.bot, Some(bot.clone()));
    assert_eq!((owner.store_calls(), fs.creates().len()), (calls, creates));
    for other in [Some(12), None] {
        assert_eq!(
            h.creation(with_avatar(&command, other)).unwrap_err().code,
            "request_conflict"
        );
    }
    // Restarted over the same database: still the same Bot, same face.
    let repo = h.stop();
    assert_eq!(repo.bot(bot.id).unwrap(), Some(bot.clone()));
    let h = start(repo, &owner, &fs);
    let replayed = h.creation(command).unwrap();
    assert_eq!(replayed.bot, Some(bot));
    assert_eq!((owner.store_calls(), fs.creates().len()), (calls, creates));
    h.stop();
}

#[test]
fn a_creation_without_a_face_replays_exactly_and_its_bot_wears_the_derived_one() {
    let owner = FakeOwner::new();
    let fs = FakeFs::with(&[root()]);
    let h = start(Repository::in_memory().unwrap(), &owner, &fs);
    let command = create(&owner, authored(), BotFolderChoice::Managed {});
    // The request an older window sends: no avatar anywhere in its bytes.
    assert!(!serde_json::to_string(&command).unwrap().contains("avatar"));
    let (request, bot_id) = request_of(&command);
    h.creation(command.clone()).unwrap();
    let done = h.settle(request);
    assert_eq!(done.stage, BotCreationStage::Completed, "{done:?}");
    let bot = done.bot.clone().expect("the completed Bot");
    assert_eq!(bot.config.avatar, None);
    assert_eq!(bot.avatar(), derived_avatar(bot_id));
    assert!(bot.avatar() < BOT_AVATAR_COUNT);
    let (calls, creates) = (owner.store_calls(), fs.creates().len());
    // Replayed as it was sent, and as the same input spelled with an
    // explicit absent face: the same completed Bot, nothing made again.
    for replay in [command.clone(), with_avatar(&command, None)] {
        let again = h.creation(replay).unwrap();
        assert_eq!(again.stage, BotCreationStage::Completed);
        assert_eq!(again.bot, Some(bot.clone()));
    }
    assert_eq!((owner.store_calls(), fs.creates().len()), (calls, creates));
    // The same request now naming a face is a different request.
    assert_eq!(
        h.creation(with_avatar(&command, Some(0))).unwrap_err().code,
        "request_conflict"
    );
    // And after a restart.
    let repo = h.stop();
    let h = start(repo, &owner, &fs);
    assert_eq!(h.creation(command).unwrap().bot, Some(bot));
    h.stop();
}

#[test]
fn a_face_outside_the_table_is_refused_before_any_intent() {
    let owner = FakeOwner::new();
    let fs = FakeFs::with(&[root()]);
    let h = start(Repository::in_memory().unwrap(), &owner, &fs);
    let command = with_avatar(
        &create(&owner, authored(), BotFolderChoice::Managed {}),
        Some(BOT_AVATAR_COUNT),
    );
    let (request, _) = request_of(&command);
    let creates = fs.creates().len();
    assert_eq!(h.creation(command).unwrap_err().code, "invalid_request");
    assert_eq!(h.read(request).unwrap_err().code, "not_found");
    assert_eq!(owner.store_calls(), 0);
    assert_eq!(fs.creates().len(), creates);
    h.stop();
}
