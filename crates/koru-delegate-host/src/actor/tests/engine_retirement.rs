// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_delegate_runtime::RunStore;

#[test]
fn engine_mode_refuses_bot_admission_before_any_provider_or_document_work() {
    let (mut actor, _, inbox) = actor();
    actor.legacy_bots = false;
    actor.wake_bots();
    assert!(inbox.try_recv().is_err());
    let result = actor.bot_command(koru_delegate_api::bots::BotCommand::Submit {
        submission: bot_submission(),
    });
    assert!(matches!(result, Err(failure) if failure.code == "root_engine_owned"));
}

#[test]
fn retired_bot_intents_are_preserved_but_never_replayed_by_ordinary_restart() {
    let mut db = RunsDb::open(std::path::Path::new(":memory:")).unwrap();
    let bot = bot_submission();
    let mut ordinary = bot.request.clone();
    ordinary.run = RunId::mint();
    ordinary.scope = WorkspaceScope::Temporary;
    db.record_intent(&bot.request, bot.workspace).unwrap();
    db.record_intent(&ordinary, bot.workspace).unwrap();
    let mut ledger = crate::ledger::SharedLedger::new(db).with_legacy_bots(false);
    assert_eq!(ledger.pending_intents().unwrap().len(), 1);
    assert_eq!(ledger.pending_intents().unwrap()[0].run, ordinary.run);
    assert!(ledger.intent(bot.request.run).unwrap().is_none());
    assert_eq!(ledger.intents().unwrap().len(), 1);
    assert!(ledger.bots().is_none());
    assert!(ledger.native_results().is_none());
    assert!(ledger.private_effects().is_none());
    assert_eq!(ledger.with_legacy_bots(true).intents().unwrap().len(), 2);
}
