// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_ai_core::bots::*;

fn operation(reply: BotResponse) -> BotOperation {
    let BotResponse::Operation { operation } = reply else {
        panic!("the existing settings form requires an operation acknowledgement")
    };
    operation
}
#[test]
fn access_and_pause_answer_the_settings_forms_operation_contract_and_remain_readable() {
    let f = Fixture::new();
    let bot = creation(f.gateway.window_command(form(&f)).unwrap())
        .bot
        .unwrap();
    let request_id = BotRequestId::from_uuid(Uuid::new_v4());
    let change = BotCommand::UpdateAccess {
        request_id,
        bot_id: bot.id,
        expected_revision: 1,
        expected_policy_revision: 1,
        access: BotAccess::Workspace,
        selected_pages: vec![bot.config.role.clone()],
        connections: None,
        may_ask: Some(false),
    };
    let saved = operation(f.gateway.window_command(change.clone()).unwrap());
    assert_eq!(saved.kind, BotOperationKind::UpdateAccess);
    assert_eq!(saved.status, BotOperationStatus::Accepted);
    let BotResponse::Bot { bot: current } = f
        .gateway
        .window_command(BotCommand::Read { bot_id: bot.id })
        .unwrap()
    else {
        panic!()
    };
    assert!(!current.may_ask);
    assert_eq!(current.policy_revision, 2);
    assert_eq!(current.access, BotAccess::Workspace);
    assert_eq!(current.selected_pages, vec![bot.config.role]);
    f.store.deleted.store(true, Ordering::Relaxed);
    assert_eq!(operation(f.gateway.window_command(change).unwrap()), saved);
    assert_eq!(
        operation(
            f.gateway
                .window_command(BotCommand::ReadOperation {
                    request_id,
                    bot_id: bot.id
                })
                .unwrap()
        ),
        saved
    );
    let pause_id = BotRequestId::from_uuid(Uuid::new_v4());
    let paused = operation(
        f.gateway
            .window_command(BotCommand::Pause {
                request_id: pause_id,
                bot_id: bot.id,
                expected_revision: 2,
            })
            .unwrap(),
    );
    assert_eq!(paused.kind, BotOperationKind::Pause);
    let resumed = operation(
        f.gateway
            .window_command(BotCommand::Resume {
                request_id: BotRequestId::from_uuid(Uuid::new_v4()),
                bot_id: bot.id,
                expected_revision: 3,
            })
            .unwrap(),
    );
    assert_eq!(resumed.kind, BotOperationKind::Resume);
    assert_eq!(
        operation(
            f.gateway
                .window_command(BotCommand::ReconcileOperation {
                    request_id: pause_id,
                    bot_id: bot.id
                })
                .unwrap()
        ),
        paused
    );
    assert_eq!(
        f.gateway
            .window_command(BotCommand::ReadOperation {
                request_id: BotRequestId::from_uuid(Uuid::new_v4()),
                bot_id: bot.id
            })
            .err(),
        Some(Error::NotAllowed)
    );
}
