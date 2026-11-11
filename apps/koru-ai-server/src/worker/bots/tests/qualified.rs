// SPDX-License-Identifier: MPL-2.0
//! M1 runs on Codex only, enforced by the owner from one constant.
use super::*;

fn claude() -> AiProviderId {
    AiProviderId::parse("agent-claude").unwrap()
}

#[test]
fn every_qualified_runtime_is_a_registered_agent_and_today_only_codex() {
    assert_eq!(QUALIFIED_BOT_RUNTIMES, &["agent-codex"]);
    for id in QUALIFIED_BOT_RUNTIMES {
        let provider = AiProviderId::parse(*id).unwrap();
        assert!(koru_ai_provider_agent::is_agent_provider(&provider), "{id}");
        assert!(qualified_bot_runtime(&provider));
    }
    assert!(!qualified_bot_runtime(&claude()));
    assert!(!qualified_bot_runtime(
        &AiProviderId::parse("openai").unwrap()
    ));
}

#[test]
fn an_unqualified_runtime_is_refused_before_discovery_or_any_write() {
    let (mut repo, authority, command) = fixture();
    let BotCommand::Create {
        request_id,
        bot_id,
        home_workspace,
        config,
        access,
        selected_pages,
    } = command.clone()
    else {
        panic!("fixture is a Create")
    };
    let mut unqualified = config.clone();
    unqualified.model.provider = claude();
    let refused = BotCommand::Create {
        request_id,
        bot_id,
        home_workspace,
        config: unqualified,
        access,
        selected_pages,
    };
    assert_eq!(
        execute_bot_command(
            &mut repo,
            authority.clone(),
            |_| panic!("an unqualified runtime is not discovered"),
            context(true),
            refused,
        )
        .unwrap_err()
        .code,
        "unsupported_capability"
    );
    // Nothing was written: no Bot, no main Chat, no receipt holding the IDs.
    assert_eq!(repo.bot(bot_id).unwrap(), None);
    assert!(repo.all_bot_chats().unwrap().is_empty());
    assert_eq!(
        repo.bot_receipt(authority.owner.user_scope, request_id, "any")
            .unwrap(),
        None
    );
    // The same identities then create the Bot on Codex.
    let bot = created(&mut repo, authority.clone(), command);
    assert_eq!(bot.id, bot_id);

    let mut changed = bot.config.clone();
    changed.model.provider = claude();
    assert_eq!(
        execute_bot_command(
            &mut repo,
            authority,
            |_| panic!("an unqualified runtime is not discovered"),
            context(true),
            BotCommand::Update {
                request_id: BotRequestId::mint(),
                bot_id: bot.id,
                expected_revision: bot.revision,
                config: changed,
            },
        )
        .unwrap_err()
        .code,
        "unsupported_capability"
    );
    assert_eq!(repo.bot(bot.id).unwrap(), Some(bot));
}

#[test]
fn a_bot_stored_on_another_runtime_still_reads_but_cannot_be_changed() {
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    // Written as an earlier build allowed: any registered agent.
    let mut legacy = bot.clone();
    legacy.config.model.provider = claude();
    legacy.revision = 2;
    repo.save_bot(
        &legacy,
        authority.profile,
        BotRequestId::mint(),
        "legacy",
        Some(1),
    )
    .unwrap();
    let BotResponse::Bot { bot: read } = execute_bot_command(
        &mut repo,
        authority.clone(),
        |_| panic!("a read discovers nothing"),
        context(true),
        BotCommand::Read { bot_id: bot.id },
    )
    .unwrap() else {
        panic!("Bot")
    };
    assert_eq!(read.config.model.provider, claude());
    let mut renamed = read.config.clone();
    renamed.name = "Renamed".into();
    assert_eq!(
        execute_bot_command(
            &mut repo,
            authority,
            |_| panic!("an unqualified runtime is not discovered"),
            context(true),
            BotCommand::Update {
                request_id: BotRequestId::mint(),
                bot_id: bot.id,
                expected_revision: read.revision,
                config: renamed,
            },
        )
        .unwrap_err()
        .code,
        "unsupported_capability"
    );
}
