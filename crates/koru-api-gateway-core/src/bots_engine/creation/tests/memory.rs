// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_ai_core::bots::*;

#[test]
fn memory_binding_preserves_library_and_replays_after_later_edits() {
    let f = Fixture::new();
    let bot = creation(f.gateway.window_command(form(&f)).unwrap())
        .bot
        .unwrap();
    let memory = bot.config.role.clone();
    let command = BotCommand::SetMemory {
        request_id: BotRequestId::mint(),
        bot_id: bot.id,
        expected_revision: bot.revision,
        memory: Some(memory.clone()),
    };
    let BotResponse::Bot { bot: saved } = f.gateway.window_command(command.clone()).unwrap() else {
        panic!()
    };
    assert_eq!(saved.revision, bot.revision + 1);
    assert_eq!(saved.config.role, bot.config.role);
    let knowledge = vec![BotPage {
        workspace: f.store.home,
        document: DocumentId::new(),
    }];
    f.gateway
        .window_command(BotCommand::SetKnowledge {
            request_id: BotRequestId::mint(),
            bot_id: bot.id,
            expected_revision: saved.revision,
            pages: knowledge.clone(),
        })
        .unwrap();
    // An already accepted operation stays replayable if its page disappears.
    f.store.deleted.store(true, Ordering::Relaxed);
    let BotResponse::Bot { bot: replay } = f.gateway.window_command(command).unwrap() else {
        panic!()
    };
    assert_eq!(replay.revision, saved.revision);
    let BotResponse::Library { library } = f
        .gateway
        .window_command(BotCommand::ReadLibrary { bot_id: bot.id })
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(library.role, bot.config.role);
    assert_eq!(library.memory, Some(memory));
    assert_eq!(library.knowledge, knowledge);
    assert!(
        f.gateway
            .window_command(BotCommand::SetMemory {
                request_id: BotRequestId::mint(),
                bot_id: bot.id,
                expected_revision: bot.revision,
                memory: None,
            })
            .is_err()
    );
}

#[test]
fn memory_binding_requires_current_page_and_workspace_authority() {
    let f = Fixture::new();
    let bot = creation(f.gateway.window_command(form(&f)).unwrap())
        .bot
        .unwrap();
    for page in [
        BotPage {
            workspace: WorkspaceId::new(),
            document: bot.config.role.document,
        },
        bot.config.role.clone(),
    ] {
        f.store.deleted.store(true, Ordering::Relaxed);
        assert!(
            f.gateway
                .window_command(BotCommand::SetMemory {
                    request_id: BotRequestId::mint(),
                    bot_id: bot.id,
                    expected_revision: bot.revision,
                    memory: Some(page),
                })
                .is_err()
        );
    }
    let BotResponse::Library { library } = f
        .gateway
        .window_command(BotCommand::ReadLibrary { bot_id: bot.id })
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(library.revision, bot.revision);
    assert_eq!(library.memory, None);
}
