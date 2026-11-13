// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::bots::link::{Answer, BotsLink, Command};
use koru_ai_core::bots::*;
use koru_draw::el::Key;
use std::sync::mpsc::{Receiver, SyncSender};

fn loaded(
    bot: &Bot,
    state: BotMemoryStatusState,
) -> (BotView, Receiver<Command>, SyncSender<Answer>) {
    let (link, commands, answers) = BotsLink::test_pair();
    let mut view = BotView::with_link(bot.id, bot.main_chat, link);
    view.enable_memory_for_test();
    view.set_viewport(800.0, 1600.0);
    view.shown_pass(None);
    let seq = commands
        .try_iter()
        .find_map(|command| {
            if let Command::Read { seq, .. } = command {
                Some(seq)
            } else {
                None
            }
        })
        .unwrap();
    answers
        .send(Answer::Read {
            seq,
            bot: bot.id,
            result: Ok(bot.clone()),
        })
        .unwrap();
    view.shown_pass(None);
    while commands.try_recv().is_ok() {}
    view.set_settings_open(true);
    view.shown_pass(None);
    let request = commands
        .try_iter()
        .find_map(|command| {
            if let Command::Memory(request) = command {
                Some(request)
            } else {
                None
            }
        })
        .unwrap();
    answers
        .send(Answer::Memory {
            request,
            result: Ok(BotResponse::MemoryStatus {
                status: BotMemoryStatus {
                    bot_id: bot.id,
                    revision: bot.revision,
                    state,
                },
            }),
        })
        .unwrap();
    view.shown_pass(None);
    while commands.try_recv().is_ok() {}
    (view, commands, answers)
}
#[test]
fn memory_host_confirmation_refuses_same_tab_replacement_and_cancel_restores_focus() {
    let (mut v, mut w) = container();
    let bot = crate::bots::tests::bot(BotId::mint(), WorkspaceId::new(), "Scout", false);
    let state = BotMemoryStatusState::Candidate {
        creation_request: BotRequestId::mint(),
        candidate: BotPage {
            workspace: bot.home_workspace,
            document: koru_ai_core::DocumentId::new(),
        },
        state: BotMemoryState::Prepared,
        observed_updated_at_ms: 7,
        refusal: None,
        can_retry: true,
        can_abandon: true,
    };
    let (view, commands, _answers) = loaded(&bot, state.clone());
    v.tabs_mut().push(Tab::bot(view));
    v.relayout();
    let abandon = Key::model(bot.id.as_uuid().as_u128(), "memory-abandon");
    let TabView::Bot(view) = &mut v.tabs_mut().active_mut().view else {
        panic!()
    };
    assert!(view.accessibility_action(abandon, true));
    assert!(v.drain_bot_memory_asks());
    assert!(v.confirm.is_some());
    assert!(View::key(
        &mut v,
        &mut w.ws,
        &winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape),
        None,
        winit::keyboard::ModifiersState::empty()
    ));
    assert!(v.confirm.is_none());
    assert!(commands.try_recv().is_err());
    let TabView::Bot(view) = &mut v.tabs_mut().active_mut().view else {
        panic!()
    };
    assert_eq!(view.focus(), Some(abandon));
    assert!(view.accessibility_action(abandon, true));
    assert!(v.drain_bot_memory_asks());
    let (replacement, replacement_commands, _replacement_answers) = loaded(&bot, state);
    v.tabs_mut().active_mut().view = TabView::Bot(replacement);
    assert!(v.settle_confirm(true));
    assert!(commands.try_recv().is_err());
    assert!(replacement_commands.try_recv().is_err());
    let TabView::Bot(view) = &v.tabs().active_tab().view else {
        panic!()
    };
    assert_ne!(view.focus(), Some(abandon));
}
