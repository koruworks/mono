// SPDX-License-Identifier: MPL-2.0
//! A Bot's defaults seed new conversations and never rewrite an existing
//! one, the main conversation included (QUESTIONS O23).
use super::*;

/// What a conversation shows and runs with, apart from the Bot label (whose
/// name is the Bot's current name by design).
fn settings(
    repo: &Repository,
    chat: ChatId,
) -> (
    String,
    ModelSelection,
    Option<String>,
    Option<bool>,
    Option<String>,
    i64,
) {
    let chat = repo
        .chat_with_access(chat, &ChatReadAccess::authorized(vec![chat]))
        .unwrap()
        .unwrap();
    (
        chat.title,
        chat.current_model,
        chat.agent_mode,
        chat.agent_fast,
        chat.agent_working_folder,
        chat.updated_at.millis(),
    )
}

fn update(repo: &mut Repository, authority: &Arc<Authority>, bot: &Bot, config: BotConfig) -> Bot {
    let BotResponse::Bot { bot } = execute_bot_command(
        repo,
        authority.clone(),
        |_| Ok(()),
        context(true),
        BotCommand::Update {
            request_id: BotRequestId::mint(),
            bot_id: bot.id,
            expected_revision: bot.revision,
            config,
        },
    )
    .unwrap() else {
        panic!("Bot")
    };
    bot
}

#[test]
fn editing_a_bot_rewrites_no_conversation_and_only_new_ones_take_the_defaults() {
    let (mut repo, authority, command) = fixture();
    let mut bot = created(&mut repo, authority.clone(), command);
    let port: Arc<dyn BotAuthority> = authority.clone();
    let BotResponse::Chat { reference } = create_bot_chat(
        &mut repo,
        &port,
        &context(true),
        BotRequestId::mint(),
        bot.id,
    )
    .unwrap() else {
        panic!("Chat reference")
    };
    let main = settings(&repo, bot.main_chat);
    let additional = settings(&repo, reference.chat_id);
    // Created from the Bot as it was.
    assert_eq!(main.0, bot.config.name);
    assert_eq!(additional.1, bot.config.model);
    // Each field on its own: name, model, reasoning, mode, Fast.
    let edits: [fn(&mut BotConfig); 5] = [
        |c| c.name = "Renamed".into(),
        |c| c.model.requested_model = "fixture-next".into(),
        |c| c.model.default_reasoning = ReasoningChoice::On,
        |c| c.mode = Some("plan".into()),
        |c| c.fast = Some(true),
    ];
    for edit in edits {
        let mut config = bot.config.clone();
        edit(&mut config);
        bot = update(&mut repo, &authority, &bot, config);
        assert_eq!(settings(&repo, bot.main_chat), main);
        assert_eq!(settings(&repo, reference.chat_id), additional);
    }
    assert_eq!(bot.config.name, "Renamed");
    // A conversation created now is seeded from the Bot's current defaults.
    let BotResponse::Chat { reference: fresh } = create_bot_chat(
        &mut repo,
        &port,
        &context(true),
        BotRequestId::mint(),
        bot.id,
    )
    .unwrap() else {
        panic!("Chat reference")
    };
    let (title, model, mode, fast, folder, _) = settings(&repo, fresh.chat_id);
    // Titled as an ordinary new chat is, never with the Bot's name (window
    // run 2); the Bot's name is on its label.
    assert_eq!(title, koru_ai_core::NEW_CHAT_TITLE);
    assert_eq!(model, bot.config.model);
    assert_eq!(model.requested_model, "fixture-next");
    assert_eq!(model.default_reasoning, ReasoningChoice::On);
    assert_eq!(mode.as_deref(), Some("plan"));
    assert_eq!(fast, Some(true));
    assert_eq!(folder.as_deref(), Some(bot.config.working_folder.as_str()));
    // Neither older conversation moved, and the main one is still the main one.
    assert_eq!(settings(&repo, bot.main_chat), main);
    assert_eq!(settings(&repo, reference.chat_id), additional);
    assert_eq!(repo.bot(bot.id).unwrap().unwrap().main_chat, bot.main_chat);
}

#[test]
fn an_access_change_rewrites_no_conversation_either() {
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    let main = settings(&repo, bot.main_chat);
    let BotResponse::Bot { bot: changed } = execute_bot_command(
        &mut repo,
        authority.clone(),
        |_| Ok(()),
        context(true),
        BotCommand::UpdateAccess {
            may_ask: None,
            request_id: BotRequestId::mint(),
            bot_id: bot.id,
            expected_revision: bot.revision,
            expected_policy_revision: bot.policy_revision,
            access: BotAccess::Workspace,
            selected_pages: vec![],
            connections: None,
        },
    )
    .unwrap() else {
        panic!("Bot")
    };
    assert_eq!(changed.access, BotAccess::Workspace);
    assert_eq!(settings(&repo, bot.main_chat), main);
}
