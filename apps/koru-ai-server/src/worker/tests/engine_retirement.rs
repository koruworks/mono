// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn engine_mode_keeps_ai_alive_but_refuses_the_direct_routine_lane() {
    let mut config = WorkerConfig::new(
        "unused-memory-fixture".into(),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .without_legacy_bots();
    config.test_repository = Some(koru_ai_sqlite::Repository::in_memory().unwrap());
    let worker = AiWorker::new(config).spawn().unwrap();
    worker.ping().unwrap();
    assert!(worker.bot_owner_server().is_none());
    assert!(worker.bot_mirror_parts.is_none());
    let result = worker.client().bots(
        context(),
        koru_ai_wire::BotCommand::ReadRoutines {
            bot_id: koru_ai_core::bots::BotId::mint(),
            after_schedule: None,
            limit: 1,
        },
    );
    assert_eq!(
        result.unwrap_err().code,
        koru_ai_core::bots::BotError::Unavailable.code()
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}
