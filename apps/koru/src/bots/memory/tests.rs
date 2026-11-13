// SPDX-License-Identifier: MPL-2.0
use super::*;
const INSTANCE: u64 = 41;
fn fixture() -> (Memory, Bot) {
    let bot = crate::bots::tests::bot(
        BotId::mint(),
        koru_store_core::ids::WorkspaceId::new(),
        "Scout",
        false,
    );
    let mut memory = Memory::default();
    memory.know(&bot);
    memory.show(true);
    (memory, bot)
}
fn status(bot: &Bot, state: BotMemoryStatusState) -> BotResponse {
    BotResponse::MemoryStatus {
        status: BotMemoryStatus {
            bot_id: bot.id,
            revision: bot.revision,
            state,
        },
    }
}
fn not_started(memory: &mut Memory, bot: &Bot) {
    let request = memory.read(INSTANCE, bot).unwrap();
    assert!(memory.answer(
        INSTANCE,
        bot,
        request,
        Ok(status(
            bot,
            BotMemoryStatusState::NotStarted { can_create: true }
        ))
    ));
}
fn candidate(bot: &Bot, state: BotMemoryState) -> BotMemoryStatusState {
    BotMemoryStatusState::Candidate {
        creation_request: BotRequestId::mint(),
        candidate: BotPage {
            workspace: bot.home_workspace,
            document: koru_ai_core::DocumentId::new(),
        },
        state,
        observed_updated_at_ms: 7,
        refusal: None,
        can_retry: state != BotMemoryState::Bound,
        can_abandon: state != BotMemoryState::Bound,
    }
}
fn bound() -> (Memory, Bot, BotMemoryNodeId) {
    let (mut memory, bot) = fixture();
    let request = memory.read(INSTANCE, &bot).unwrap();
    memory.answer(
        INSTANCE,
        &bot,
        request,
        Ok(status(&bot, candidate(&bot, BotMemoryState::Bound))),
    );
    let id = BotMemoryNodeId::from_uuid(uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, b"fact"));
    let request = memory.facts(INSTANCE, &bot, false).unwrap();
    let page = BotMemoryListPage::Observed {
        page: memory.page().unwrap().clone(),
        version: koru_delegate_core::DocumentVersionId::new(),
        counts: BotMemoryCounts {
            valid: 1,
            omitted_from_prompt: 0,
            invalid: 0,
            not_indexed: 0,
        },
        items: vec![BotMemoryFactSummary {
            fact_id: id,
            summary: "Fact".into(),
        }],
        complete: false,
        continuation: Some("list_cursor".into()),
    };
    memory.answer(
        INSTANCE,
        &bot,
        request,
        Ok(BotResponse::MemoryList { page }),
    );
    (memory, bot, id)
}
#[test]
fn memory_loading_error_is_not_not_started() {
    let (mut memory, bot) = fixture();
    assert!(!memory.can_create(&bot));
    not_started(&mut memory, &bot);
    assert!(memory.can_create(&bot));
    memory.refresh();
    let request = memory.read(INSTANCE, &bot).unwrap();
    assert!(!memory.can_create(&bot));
    memory.unsent(&request);
    assert!(!memory.can_create(&bot));
    assert!(memory.read(INSTANCE, &bot).is_none());
}
#[test]
fn memory_unknown_ensure_keeps_exact_body_across_locale_change() {
    let (mut memory, bot) = fixture();
    not_started(&mut memory, &bot);
    let first = memory.create(INSTANCE, &bot, "Mémoire".into()).unwrap();
    memory.answer(INSTANCE, &bot, first.clone(), Err(Failure::Unanswered));
    memory.show(false);
    memory.show(true);
    let retry = memory.retry_ensure(INSTANCE, &bot).unwrap();
    assert_eq!(first.command, retry.command);
    assert_ne!(first.serial, retry.serial);
}
#[test]
fn memory_not_started_read_does_not_replace_uncertain_ensure() {
    let (mut memory, bot) = fixture();
    not_started(&mut memory, &bot);
    let first = memory.create(INSTANCE, &bot, "Memory".into()).unwrap();
    memory.answer(INSTANCE, &bot, first.clone(), Err(Failure::Unanswered));
    memory.refresh();
    not_started(&mut memory, &bot);
    assert!(!memory.can_create(&bot));
    assert_eq!(
        memory.retry_ensure(INSTANCE, &bot).unwrap().command,
        first.command
    );
}
#[test]
fn memory_retry_abandon_unknown_requires_new_explicit_action() {
    let (mut memory, bot) = fixture();
    let state = candidate(&bot, BotMemoryState::Prepared);
    let read = memory.read(INSTANCE, &bot).unwrap();
    memory.answer(INSTANCE, &bot, read, Ok(status(&bot, state.clone())));
    let retry = memory.retry(INSTANCE, &bot).unwrap();
    memory.answer(INSTANCE, &bot, retry.clone(), Err(Failure::Unanswered));
    assert_eq!(memory.notice, Some(Notice::Reconcile));
    assert!(!memory.can_retry(&bot));
    let read = memory.read(INSTANCE, &bot).unwrap();
    memory.answer(INSTANCE, &bot, read, Ok(status(&bot, state)));
    assert!(memory.read(INSTANCE, &bot).is_none());
    let ask = memory.ask_abandon(INSTANCE, &bot).unwrap();
    assert!(memory.confirm(INSTANCE + 1, &bot, &ask, true).is_none());
    let command = memory.confirm(INSTANCE, &bot, &ask, true).unwrap();
    assert_eq!(command.kind, Kind::Abandon);
    assert_ne!(command.serial, retry.serial);
    assert!(!memory.answer(INSTANCE, &bot, retry, Err(Failure::Unanswered)));
    assert!(memory.busy(), "an old Retry reply cannot settle Abandon");
}
#[test]
fn memory_confirmation_cannot_apply_new_token() {
    let (mut memory, bot) = fixture();
    let state = candidate(&bot, BotMemoryState::Prepared);
    let read = memory.read(INSTANCE, &bot).unwrap();
    memory.answer(INSTANCE, &bot, read, Ok(status(&bot, state)));
    let ask = memory.ask_abandon(INSTANCE, &bot).unwrap();
    if let BotMemoryStatusState::Candidate {
        observed_updated_at_ms,
        ..
    } = &mut memory.status.as_mut().unwrap().state
    {
        *observed_updated_at_ms += 1;
    }
    assert!(memory.confirm(INSTANCE, &bot, &ask, true).is_none());
    assert!(!memory.busy());
    assert_eq!(memory.notice, Some(Notice::Reconcile));
}
#[test]
fn memory_stale_status_coalesces_one_reconciliation() {
    let (mut memory, mut bot) = fixture();
    not_started(&mut memory, &bot);
    let old = bot.clone();
    bot.revision += 1;
    memory.know(&bot);
    for _ in 0..2 {
        let read = memory.read(INSTANCE, &bot).unwrap();
        memory.answer(
            INSTANCE,
            &bot,
            read,
            Ok(status(
                &old,
                BotMemoryStatusState::NotStarted { can_create: true },
            )),
        );
    }
    assert!(memory.read(INSTANCE, &bot).is_none());
    memory.refresh();
    assert!(memory.read(INSTANCE, &bot).is_some());
}
#[test]
fn memory_back_holds_physical_detail_slot_until_reply() {
    let (mut memory, bot, id) = bound();
    let detail = memory.fact(INSTANCE, &bot, id, false).unwrap();
    memory.back();
    assert!(memory.busy());
    assert!(memory.facts(INSTANCE, &bot, false).is_none());
    memory.answer(
        INSTANCE,
        &bot,
        detail,
        Ok(BotResponse::MemoryDetail {
            page: BotMemoryDetailPage::Absent {},
        }),
    );
    assert!(!memory.busy());
    assert!(memory.detail.is_none());
}
#[test]
fn memory_detail_owns_its_version_and_expiry_has_no_latest_fallback() {
    let (mut memory, bot, id) = bound();
    let version = koru_delegate_core::DocumentVersionId::new();
    let request = memory.fact(INSTANCE, &bot, id, false).unwrap();
    memory.answer(
        INSTANCE,
        &bot,
        request,
        Ok(BotResponse::MemoryDetail {
            page: BotMemoryDetailPage::Observed {
                page: memory.page().unwrap().clone(),
                version,
                fact_id: id,
                chunks: vec![],
                complete: false,
                continuation: Some("detail_cursor".into()),
            },
        }),
    );
    assert!(
        matches!(&memory.detail, Some(BotMemoryDetailPage::Observed { version: v, .. }) if *v == version)
    );
    let request = memory.fact(INSTANCE, &bot, id, true).unwrap();
    memory.answer(
        INSTANCE,
        &bot,
        request,
        Err(Failure::Refused("memory_cursor_expired".into())),
    );
    assert!(memory.detail.is_some());
    assert!(memory.detail_stale);
    assert!(memory.fact(INSTANCE, &bot, id, true).is_none());
    assert!(memory.read(INSTANCE, &bot).is_none());
}
#[test]
fn memory_foreign_or_stale_reply_is_ignored() {
    let (mut memory, bot) = fixture();
    let request = memory.read(INSTANCE, &bot).unwrap();
    let mut wrong = request.clone();
    wrong.kind = Kind::Retry;
    assert!(!memory.answer(
        INSTANCE,
        &bot,
        wrong,
        Ok(status(
            &bot,
            BotMemoryStatusState::NotStarted { can_create: true }
        ))
    ));
    assert!(!memory.answer(
        INSTANCE + 1,
        &bot,
        request.clone(),
        Ok(status(
            &bot,
            BotMemoryStatusState::NotStarted { can_create: true }
        ))
    ));
    assert!(memory.busy());
    memory.unsent(&request);
    assert!(!memory.busy());
}
#[test]
fn memory_hidden_settings_send_no_reads() {
    let (mut memory, bot) = fixture();
    memory.show(false);
    assert!(memory.read(INSTANCE, &bot).is_none());
    memory.show(true);
    let request = memory.read(INSTANCE, &bot).unwrap();
    memory.unsent(&request);
    assert!(memory.read(INSTANCE, &bot).is_none());
}

#[test]
fn memory_owner_change_holds_old_physical_slot_but_discards_its_result() {
    let (mut memory, mut bot) = fixture();
    let old = bot.clone();
    let request = memory.read(INSTANCE, &bot).unwrap();
    bot.owner_device = koru_delegate_core::DeviceId::new();
    memory.know(&bot);
    memory.show(true);
    assert!(memory.busy());
    assert!(memory.read(INSTANCE, &bot).is_none());
    assert!(memory.answer(
        INSTANCE,
        &bot,
        request,
        Ok(status(
            &old,
            BotMemoryStatusState::NotStarted { can_create: true }
        ))
    ));
    assert!(memory.status.is_none());
    assert!(!memory.can_create(&bot));
    assert!(memory.read(INSTANCE, &bot).is_some());
}

#[test]
fn memory_one_page_next_replaces_and_revision_race_refuses_body() {
    let (mut memory, mut bot, id) = bound();
    let BotMemoryListPage::Observed { version, .. } = memory.list.as_ref().unwrap() else {
        panic!()
    };
    let version = *version;
    let next = memory.facts(INSTANCE, &bot, true).unwrap();
    let other = BotMemoryNodeId::from_uuid(uuid::Uuid::new_v4());
    let page = BotMemoryListPage::Observed {
        page: memory.page().unwrap().clone(),
        version,
        counts: BotMemoryCounts {
            valid: 2,
            omitted_from_prompt: 0,
            invalid: 0,
            not_indexed: 0,
        },
        items: vec![BotMemoryFactSummary {
            fact_id: other,
            summary: "Second".into(),
        }],
        complete: true,
        continuation: None,
    };
    memory.answer(INSTANCE, &bot, next, Ok(BotResponse::MemoryList { page }));
    assert!(
        matches!(&memory.list,Some(BotMemoryListPage::Observed { items,.. }) if items.len()==1 && items[0].fact_id==other && items[0].fact_id!=id)
    );
    let detail = memory.fact(INSTANCE, &bot, other, false).unwrap();
    bot.revision += 1;
    memory.know(&bot);
    memory.answer(
        INSTANCE,
        &bot,
        detail,
        Ok(BotResponse::MemoryDetail {
            page: BotMemoryDetailPage::Absent {},
        }),
    );
    assert!(memory.detail.is_none());
    assert!(memory.list_stale);
}

#[test]
fn memory_oversized_page_keeps_open_page_and_requires_explicit_retry() {
    let (mut memory, bot, _) = bound();
    let page = memory.page().unwrap().clone();
    let request = memory.facts(INSTANCE, &bot, true).unwrap();
    memory.answer(
        INSTANCE,
        &bot,
        request,
        Err(Failure::Refused("context_too_large".into())),
    );
    assert_eq!(memory.notice, Some(Notice::Unsupported));
    assert_eq!(memory.page(), Some(&page));
    assert!(memory.list_stale);
    assert!(memory.read(INSTANCE, &bot).is_none());
}
