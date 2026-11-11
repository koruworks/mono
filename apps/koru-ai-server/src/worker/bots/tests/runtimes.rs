// SPDX-License-Identifier: MPL-2.0
//! Any registered agent runtime may run a Bot (Steven's second review,
//! O33): the owner refuses none by name. Readiness is the model discovery's
//! to report, through the runtime check every Create and Update makes.
use super::*;

fn claude() -> AiProviderId {
    AiProviderId::parse("agent-claude").unwrap()
}

fn on(command: BotCommand, provider: AiProviderId) -> BotCommand {
    let BotCommand::Create {
        request_id,
        bot_id,
        home_workspace,
        mut config,
        access,
        selected_pages,
    } = command
    else {
        panic!("fixture is a Create")
    };
    config.model.provider = provider;
    BotCommand::Create {
        request_id,
        bot_id,
        home_workspace,
        config,
        access,
        selected_pages,
    }
}

#[test]
fn a_bot_is_created_and_changed_on_any_registered_agent_runtime() {
    assert!(koru_ai_provider_agent::is_agent_provider(&claude()));
    let (mut repo, authority, command) = fixture();
    let checked = std::cell::RefCell::new(Vec::new());
    let BotResponse::Bot { bot } = execute_bot_command(
        &mut repo,
        authority.clone(),
        |config| {
            checked.borrow_mut().push(config.model.provider.clone());
            Ok(())
        },
        context(true),
        on(command, claude()),
    )
    .unwrap() else {
        panic!("Bot")
    };
    assert_eq!(bot.config.model.provider, claude());
    let mut renamed = bot.config.clone();
    renamed.name = "Renamed".into();
    let BotResponse::Bot { bot: changed } = execute_bot_command(
        &mut repo,
        authority,
        |config| {
            checked.borrow_mut().push(config.model.provider.clone());
            Ok(())
        },
        context(true),
        BotCommand::Update {
            request_id: BotRequestId::mint(),
            bot_id: bot.id,
            expected_revision: bot.revision,
            config: renamed,
        },
    )
    .unwrap() else {
        panic!("Bot")
    };
    assert_eq!(changed.config.name, "Renamed");
    // Both went through the runtime check: readiness is reported, not ruled.
    assert_eq!(*checked.borrow(), vec![claude(), claude()]);
}

#[test]
fn a_runtime_that_is_not_ready_is_refused_by_its_discovery_and_nothing_is_written() {
    let (mut repo, authority, command) = fixture();
    let command = on(command, claude());
    let BotCommand::Create {
        request_id, bot_id, ..
    } = command.clone()
    else {
        unreachable!()
    };
    let unready = execute_bot_command(
        &mut repo,
        authority.clone(),
        |_| {
            Err(failure(
                "runtime_unavailable",
                "The selected runtime is unavailable.",
                true,
            ))
        },
        context(true),
        command,
    )
    .unwrap_err();
    assert_eq!(unready.code, "runtime_unavailable");
    assert_eq!(repo.bot(bot_id).unwrap(), None);
    assert!(repo.all_bot_chats().unwrap().is_empty());
    assert_eq!(
        repo.bot_receipt(authority.owner.user_scope, request_id, "any")
            .unwrap(),
        None
    );
}

#[test]
fn an_api_access_provider_is_still_not_a_bot_runtime() {
    // What existed before the Codex-only rule stays: a Bot runs an agent.
    let (mut repo, authority, command) = fixture();
    let refused = execute_bot_command(
        &mut repo,
        authority,
        |_| panic!("an invalid configuration is not discovered"),
        context(true),
        on(command, AiProviderId::parse("openai").unwrap()),
    )
    .unwrap_err();
    assert_eq!(refused.code, "invalid_request");
}
