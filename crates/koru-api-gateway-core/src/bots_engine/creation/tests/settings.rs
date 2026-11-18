// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::bots_engine::window::readiness::{Refusal, validate};
use koru_ai_core::bots::*;

#[test]
fn runtime_validation_and_chat_lists_use_the_conversations_own_selection() {
    let f = Fixture::new();
    let bot = creation(f.gateway.window_command(form(&f)).unwrap())
        .bot
        .unwrap();
    let BotResponse::Chat { reference } = f
        .gateway
        .window_command(BotCommand::CreateChat {
            bot_id: bot.id,
            request_id: BotRequestId::from_uuid(Uuid::new_v4()),
        })
        .unwrap()
    else {
        panic!()
    };
    let principal = crate::GatewayPrincipal {
        label: "synthetic".into(),
        scopes: vec![koru_ai_api::AiScope::ManageBots.as_str().into()],
    };
    let command = Command::Change {
        id: Uuid::new_v4(),
        action: Change::ConversationSettings {
            bot: bot.id.as_uuid(),
            thread: bot.main_chat.as_uuid(),
            revision: 1,
            provider: "claude".parse().unwrap(),
            selection: koru_session_protocol::LaunchSelection {
                model: Some("specific".parse().unwrap()),
                fast: Some(true),
                ..Default::default()
            },
        },
    };
    assert!(matches!(
        f.gateway
            .validated_execute(&principal, command.clone(), |c| validate(None, c)),
        Err(Refusal::Runtime(_))
    ));
    let saved = f
        .gateway
        .validated_execute(&principal, command.clone(), |c| {
            assert_eq!(c.selection.model.as_ref().unwrap().as_str(), "specific");
            assert_eq!(c.selection.fast, Some(true));
            Ok(())
        })
        .ok()
        .unwrap();
    let replay = f
        .gateway
        .validated_execute(&principal, command, |_| {
            panic!("retained choice rediscovered runtime")
        })
        .ok()
        .unwrap();
    assert_eq!(saved["answer"], replay["answer"]);
    assert_eq!(replay["replayed"], true);
    let BotResponse::Chats { page } = f
        .gateway
        .window_command(BotCommand::ListBotChats {
            bot_id: bot.id,
            cursor: None,
            limit: 100,
        })
        .unwrap()
    else {
        panic!()
    };
    let main = page
        .chats
        .iter()
        .find(|item| item.chat.id == bot.main_chat)
        .unwrap();
    let other = page
        .chats
        .iter()
        .find(|item| item.chat.id == reference.chat_id)
        .unwrap();
    assert_eq!(main.chat.current_model.requested_model, "specific");
    assert_eq!(main.chat.agent_fast, Some(true));
    assert_eq!(other.chat.current_model, bot.config.model);
    assert_eq!(other.chat.agent_fast, bot.config.fast);
}
