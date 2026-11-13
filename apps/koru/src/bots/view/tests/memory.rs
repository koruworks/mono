// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::bots::memory::{Kind, Request};
use crate::bots::view::memory::Action;
use koru_ai_core::bots::*;

fn ready() -> Fixture {
    let mut f = Fixture::opened(unpaused());
    while f.commands.try_recv().is_ok() {}
    f
}
fn request(f: &Fixture) -> Request {
    loop {
        if let Command::Memory(request) = f.commands.try_recv().expect("Memory request queued") {
            return request;
        }
    }
}
fn answer(f: &mut Fixture, request: Request, state: BotMemoryStatusState, revision: u64) {
    f.answers
        .send(Answer::Memory {
            request,
            result: Ok(BotResponse::MemoryStatus {
                status: BotMemoryStatus {
                    bot_id: f.bot.id,
                    revision,
                    state,
                },
            }),
        })
        .unwrap();
    assert!(f.view.poll());
    f.publishable("Memory status");
}
fn load(f: &mut Fixture, state: BotMemoryStatusState) {
    assert!(f.view.memory_pass());
    let req = request(f);
    answer(f, req, state, f.bot.revision);
}
fn candidate(f: &Fixture, state: BotMemoryState) -> BotMemoryStatusState {
    BotMemoryStatusState::Candidate {
        creation_request: BotRequestId::mint(),
        candidate: BotPage {
            workspace: f.bot.home_workspace,
            document: koru_ai_core::DocumentId::new(),
        },
        state,
        observed_updated_at_ms: 4,
        refusal: None,
        can_retry: state != BotMemoryState::Bound,
        can_abandon: state != BotMemoryState::Bound,
    }
}
#[test]
fn enabled_memory_normal_maintenance_schedules_and_denied_view_refuses() {
    let mut f = Fixture::opened(unpaused());
    while f.commands.try_recv().is_ok() {}
    assert!(koru_ai_core::bots::memory::BOT_MEMORY_AVAILABLE);
    assert!(!f.view.memory_test_available);
    let mut ws = WorkspaceDoc::create(koru_core::WorkspaceId::new(), "test").unwrap();
    View::maintain(&mut f.view, &mut ws, std::time::Instant::now());
    let read = request(&f);
    assert_eq!(read.kind, Kind::Status);
    let revision = f.bot.revision;
    answer(
        &mut f,
        read,
        BotMemoryStatusState::NotStarted { can_create: true },
        revision,
    );
    assert!(
        f.view
            .with_frame(|_, frame| frame.contains(f.view.memory_key("memory-section")))
    );
    while f.commands.try_recv().is_ok() {}
    f.view.load = Load::Denied;
    View::maintain(&mut f.view, &mut ws, std::time::Instant::now());
    assert!(!f.view.perform_memory(Action::Refresh));
    assert!(
        !f.commands
            .try_iter()
            .any(|command| matches!(command, Command::Memory(_)))
    );
}
#[test]
fn memory_controls_are_named_and_unknown_create_retries_exactly() {
    let mut f = ready();
    load(
        &mut f,
        BotMemoryStatusState::NotStarted { can_create: true },
    );
    let create = f.view.memory_key("memory-create");
    f.view.focus = Some(create);
    f.view.reveal_focus();
    let mut ws = WorkspaceDoc::create(koru_core::WorkspaceId::new(), "test").unwrap();
    assert!(View::key(
        &mut f.view,
        &mut ws,
        &named(winit::keyboard::NamedKey::Enter),
        None,
        ModifiersState::empty()
    ));
    let first = request(&f);
    assert_eq!(first.kind, Kind::Ensure);
    f.answers
        .send(Answer::Memory {
            request: first.clone(),
            result: Err(Failure::Unanswered),
        })
        .unwrap();
    f.view.poll();
    f.view.lang = Lang::Ja;
    let retry = f.view.memory_key("memory-retry-request");
    f.view.focus = Some(retry);
    f.view.reveal_focus();
    assert!(key(
        &mut f.view,
        named(winit::keyboard::NamedKey::Space),
        Some(" ")
    ));
    assert_eq!(request(&f).command, first.command);
    f.publishable("uncertain Memory retry");
}
#[test]
fn memory_newer_status_refreshes_canonical_bot_and_fences_old_read() {
    let mut f = ready();
    assert!(f.view.memory_pass());
    let req = request(&f);
    let state = candidate(&f, BotMemoryState::Bound);
    let revision = f.bot.revision + 1;
    answer(&mut f, req, state, revision);
    assert!(!f.view.asked);
    assert!(!f.view.memory.current(&f.bot));
    f.view.take(Ok(f.bot.clone()));
    assert!(!f.view.asked);
    f.bot.revision = revision;
    f.view.take(Ok(f.bot.clone()));
    assert!(f.view.memory.current(&f.bot));
}
#[test]
fn memory_same_tab_replacement_rejects_old_confirmation_and_reply() {
    let mut f = ready();
    let state = candidate(&f, BotMemoryState::Prepared);
    load(&mut f, state.clone());
    assert!(f.view.perform_memory(Action::Abandon));
    let ask = f.view.take_memory_ask().unwrap();
    let old_instance = f.view.context_instance;
    let (link, commands, _answers) = BotsLink::test_pair();
    let mut replacement = BotView::with_link(f.bot.id, f.bot.main_chat, link);
    replacement.take(Ok(f.bot.clone()));
    replacement.set_settings_open(true);
    assert_ne!(replacement.context_instance, old_instance);
    assert!(!replacement.settle_memory_ask(&ask, true));
    assert!(commands.try_recv().is_err());
    assert!(f.view.settle_memory_ask(&ask, false));
    assert_eq!(f.view.focus, Some(ask.focus));
}
#[test]
fn memory_v5_fact_open_uses_exact_document_target() {
    let mut f = ready();
    let state = candidate(&f, BotMemoryState::Bound);
    load(&mut f, state);
    let id = BotMemoryNodeId::from_uuid(uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, b"fact"));
    let page = f.view.memory.page().unwrap().clone();
    f.view.memory.list = Some(BotMemoryListPage::Observed {
        page: page.clone(),
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
        complete: true,
        continuation: None,
    });
    assert!(f.view.perform_memory(Action::Open(Some(id))));
    let target = f.view.take_document_request().unwrap();
    assert_eq!(target.document.as_uuid(), page.document.as_uuid());
    assert_eq!(target.node.unwrap().as_uuid(), id.as_uuid());
}
#[test]
fn memory_long_locale_and_rtl_layout_share_hit_geometry() {
    for lang in Lang::ALL {
        let mut f = ready();
        let state = candidate(&f, BotMemoryState::Bound);
        load(&mut f, state);
        f.view.lang = lang;
        f.view.set_viewport(420.0, 560.0);
        f.view.memory.list = Some(BotMemoryListPage::Observed {
            page: f.view.memory.page().unwrap().clone(),
            version: koru_delegate_core::DocumentVersionId::new(),
            counts: BotMemoryCounts {
                valid: 1,
                omitted_from_prompt: 1,
                invalid: 2,
                not_indexed: 3,
            },
            items: vec![BotMemoryFactSummary {
                fact_id: BotMemoryNodeId::from_uuid(uuid::Uuid::new_v4()),
                summary: "日本語 العربية résumé ".repeat(15),
            }],
            complete: true,
            continuation: None,
        });
        f.view.focus = Some(f.view.memory_key("memory-start"));
        f.view.reveal_focus();
        assert!(f.view.scroll > 0.0);
        f.view.with_frame(|_, frame| {
            for node in frame.nodes.iter().filter(|node| {
                node.key.is_some_and(|key| key.part.starts_with("memory-")) && node.focusable
            }) {
                assert!(node.label.is_some());
                assert!(node.rect.w <= 420.0);
            }
        });
        f.publishable("localized Memory controls");
    }
}

#[test]
fn memory_bound_empty_absent_and_invalid_counts_differ() {
    let mut f = ready();
    let state = candidate(&f, BotMemoryState::Bound);
    load(&mut f, state);
    f.view.memory.list = Some(BotMemoryListPage::Absent {});
    assert!(
        f.view
            .with_frame(|_, frame| frame.contains(f.view.memory_key("memory-absent")))
    );
    for (invalid, not_indexed, empty) in [(0, 0, true), (1, 0, false), (0, 1, false)] {
        f.view.memory.list = Some(BotMemoryListPage::Observed {
            page: f.view.memory.page().unwrap().clone(),
            version: koru_delegate_core::DocumentVersionId::new(),
            counts: BotMemoryCounts {
                valid: 0,
                omitted_from_prompt: 0,
                invalid,
                not_indexed,
            },
            items: vec![],
            complete: true,
            continuation: None,
        });
        f.view.with_frame(|_, frame| {
            assert!(!frame.contains(f.view.memory_key("memory-absent")));
            assert!(frame.contains(f.view.memory_key("memory-counts")));
            assert_eq!(frame.contains(f.view.memory_key("memory-empty")), empty);
        });
    }
}
