// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_ai_core::bots::*;

fn operation(response: BotResponse) -> BotLifecycleProgress {
    let BotResponse::Lifecycle { operation } = response else {
        panic!()
    };
    operation
}
fn bot(f: &Fixture, id: BotId) -> Bot {
    let BotResponse::Bot { bot } = f
        .gateway
        .window_command(BotCommand::Read { bot_id: id })
        .unwrap()
    else {
        panic!()
    };
    bot
}
#[test]
fn existing_lifecycle_controls_recover_exact_holders_and_restore_archive_under_trash() {
    let f = Fixture::new();
    let created = creation(f.gateway.window_command(form(&f)).unwrap())
        .bot
        .unwrap();
    let archive = BotRequestId::from_uuid(Uuid::new_v4());
    let archived = operation(
        f.gateway
            .window_command(BotCommand::Archive {
                request_id: archive,
                bot_id: created.id,
                expected_revision: 1,
            })
            .unwrap(),
    );
    assert_eq!(archived.kind, BotLifecycleKind::Archive);
    assert_eq!(archived.stage, BotLifecycleStage::Completed);
    let held = bot(&f, created.id);
    assert_eq!(held.held_by.unwrap().request_id, archive);
    assert!(held.paused);
    let trash = BotRequestId::from_uuid(Uuid::new_v4());
    let trashed = operation(
        f.gateway
            .window_command(BotCommand::Trash {
                request_id: trash,
                bot_id: created.id,
                expected_revision: 2,
            })
            .unwrap(),
    );
    assert!(trashed.left_in_place.contains(&BotLeftInPlace::Pages));
    let restore = BotCommand::Restore {
        request_id: BotRequestId::from_uuid(Uuid::new_v4()),
        bot_id: created.id,
        expected_revision: 3,
        source_request_id: trash,
    };
    assert_eq!(
        operation(f.gateway.window_command(restore.clone()).unwrap()).kind,
        BotLifecycleKind::Restore
    );
    let held = bot(&f, created.id);
    assert_eq!(held.lifecycle, BotLifecycle::Archived);
    assert_eq!(held.held_by.unwrap().request_id, archive);
    f.gateway.window_command(restore).unwrap();
    let reversed = operation(
        f.gateway
            .window_command(BotCommand::ReadLifecycleOperation {
                request_id: trash,
                bot_id: created.id,
            })
            .unwrap(),
    );
    assert_eq!(reversed.stage, BotLifecycleStage::Reversed);
    f.gateway
        .window_command(BotCommand::Restore {
            request_id: BotRequestId::from_uuid(Uuid::new_v4()),
            bot_id: created.id,
            expected_revision: 4,
            source_request_id: archive,
        })
        .unwrap();
    let active = bot(&f, created.id);
    assert_eq!(active.lifecycle, BotLifecycle::Active);
    assert!(active.held_by.is_none() && active.paused);
    assert_eq!(f.store.writes.load(Ordering::Relaxed), 1);
}

#[test]
fn window_conversation_lifecycle_uses_bot_revision_and_retries_after_thread_revision_moves() {
    let f = Fixture::new();
    let created = creation(f.gateway.window_command(form(&f)).unwrap())
        .bot
        .unwrap();
    let create = BotCommand::CreateChat {
        request_id: BotRequestId::from_uuid(Uuid::new_v4()),
        bot_id: created.id,
    };
    let BotResponse::Chat { reference } = f.gateway.window_command(create.clone()).unwrap() else {
        panic!()
    };
    let BotResponse::Chat {
        reference: repeated,
    } = f.gateway.window_command(create).unwrap()
    else {
        panic!()
    };
    assert_eq!(reference, repeated);
    let trash_id = BotRequestId::from_uuid(Uuid::new_v4());
    let trash = BotCommand::TrashChat {
        request_id: trash_id,
        bot_id: created.id,
        chat_id: reference.chat_id,
        expected_revision: 1,
    };
    let first = operation(f.gateway.window_command(trash.clone()).unwrap());
    assert_eq!(first.kind, BotLifecycleKind::TrashChat);
    assert_eq!(
        operation(f.gateway.window_command(trash.clone()).unwrap()),
        first
    );
    let restore = BotCommand::RestoreChat {
        request_id: BotRequestId::from_uuid(Uuid::new_v4()),
        bot_id: created.id,
        chat_id: reference.chat_id,
        expected_revision: 1,
        source_request_id: trash_id,
    };
    let restored = operation(f.gateway.window_command(restore.clone()).unwrap());
    assert_eq!(restored.kind, BotLifecycleKind::RestoreChat);
    assert_eq!(
        operation(f.gateway.window_command(restore).unwrap()),
        restored
    );
    assert_eq!(
        operation(f.gateway.window_command(trash).unwrap()).stage,
        BotLifecycleStage::Reversed
    );
    assert_eq!(bot(&f, created.id).revision, 1);
    assert_eq!(
        f.gateway
            .window_command(BotCommand::TrashChat {
                request_id: BotRequestId::from_uuid(Uuid::new_v4()),
                bot_id: created.id,
                chat_id: reference.chat_id,
                expected_revision: 2
            })
            .err(),
        Some(Error::RevisionConflict)
    );
    assert_eq!(
        f.gateway
            .window_command(BotCommand::TrashChat {
                request_id: BotRequestId::from_uuid(Uuid::new_v4()),
                bot_id: created.id,
                chat_id: created.main_chat,
                expected_revision: 1
            })
            .err(),
        Some(Error::NotAllowed)
    );
}
