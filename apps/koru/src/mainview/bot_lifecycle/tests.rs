// SPDX-License-Identifier: MPL-2.0

//! The window's half of a Bot's lifecycle (S14): its menu and its question,
//! nothing sent before a yes, the keyboard's return, and the Trash's Bot
//! rows — Restore only where it can be asked for, never a permanent delete,
//! and an Empty Trash that says what it leaves.

use std::sync::mpsc::Receiver;

use koru_ai_core::bots::{Bot, BotCommand, BotId, BotLifecycleKind, BotRequestId};
use koru_store_core::ids::WorkspaceId;

use crate::bots::BotView;
use crate::bots::lifecycle::Ask;
use crate::bots::link::{BotsLink, Command};
use crate::mainview::MainView;
use crate::mainview::context_menu::{MenuAction, MenuSubject};
use crate::mainview::harness::*;
use crate::mainview::tab::{Tab, TabView};
use crate::mainview::trash::ConfirmSubject;

/// A window with a Bot tab in front, over a link the test holds.
fn with_bot_tab() -> (MainView, Receiver<Command>, Bot) {
    let (mut v, _w) = container();
    let bot = crate::bots::tests::bot(BotId::mint(), WorkspaceId::new(), "Scout", false);
    let (link, commands, _answers) = BotsLink::test_pair();
    // The answers' end stays open for the test's life.
    std::mem::forget(_answers);
    let mut view = BotView::with_link(bot.id, bot.main_chat, link);
    koru_workspace_host::View::set_viewport(&mut view, 800.0, 600.0);
    view.know_bot_for_test(bot.clone());
    v.tabs_mut().push(Tab::bot(view));
    (v, commands, bot)
}

fn writes(commands: &Receiver<Command>) -> Vec<BotCommand> {
    commands
        .try_iter()
        .filter_map(|command| match command {
            Command::Write { command, .. } => Some(*command),
            _ => None,
        })
        .collect()
}

/// **Archive from the menu asks first, in the person's words; Cancel sends
/// nothing; a yes sends the Archive from the Bot's tab.**
#[test]
fn archive_is_asked_about_and_sent_only_on_yes() {
    let (mut v, commands, bot) = with_bot_tab();
    assert!(v.choose_bot_lifecycle(bot.id, MenuAction::ArchiveBot));
    let (dialog, subject) = v.confirm.as_ref().expect("it asks");
    assert_eq!(dialog.title(), "Archive “Scout”?");
    assert!(
        dialog
            .body()
            .contains("including a session you have taken over")
    );
    assert_eq!(
        *subject,
        ConfirmSubject::BotLifecycle {
            bot: bot.id,
            ask: Ask::Archive
        }
    );
    assert!(writes(&commands).is_empty(), "nothing before the answer");
    v.settle_confirm(false);
    assert!(v.confirm.is_none());
    assert!(writes(&commands).is_empty(), "a no sends nothing");

    v.choose_bot_lifecycle(bot.id, MenuAction::TrashBot);
    let (dialog, _) = v.confirm.as_ref().expect("it asks");
    assert!(
        dialog
            .body()
            .contains("role page, results and working folder stay where they are")
    );
    v.settle_confirm(true);
    let sent = writes(&commands);
    assert!(
        matches!(sent.as_slice(), [BotCommand::Trash { bot_id, .. }] if *bot_id == bot.id),
        "{sent:?}"
    );
}

/// **The menu has only the rows the tab offers**, and a conversation's
/// row dims while another operation of the Bot is on its way.
#[test]
fn the_menus_rows_are_the_tabs() {
    let (mut v, _commands, bot) = with_bot_tab();
    let TabView::Bot(view) = &mut v.tabs_mut().active_mut().view else {
        panic!("a Bot tab");
    };
    assert_eq!(view.lifecycle_rows(), [Ask::Archive, Ask::Trash]);
    let chat = bot.main_chat;
    assert!(view.trash_conversation(chat));
    let menu = crate::mainview::context_menu::ContextMenu::new(
        &mut v.engine,
        MenuSubject::BotConversation { bot: bot.id, chat },
        (10.0, 10.0),
        vec![MenuAction::TrashBotConversation],
    );
    assert_eq!(v.enabled_items(&menu), [false], "waits, saying why");
}

/// **The Trash's Bot rows**: a Bot conversation the owner lists in Trash
/// is a row, restorable as the owner says; no
/// row of them is deleted for good, and Empty Trash says it leaves them.
#[test]
fn bot_rows_in_the_trash_restore_never_delete_and_the_empty_says_so() {
    let (mut v, mut w) = container();
    let home = WorkspaceId::new();
    v.bot_trash.set_workspace(Some(home));
    let bot = BotId::mint();
    let chat = koru_ai_ui::ChatId::new();
    v.bot_trash.listed_for_test(
        Vec::new(),
        vec![crate::bots::tests::trashed_chat(
            bot,
            "Writer",
            chat,
            "Notes",
            (BotRequestId::mint(), BotLifecycleKind::TrashChat),
            koru_ai_core::bots::BotChatRestore::Conversation,
        )],
    );
    let rows = v.bot_trash.rows();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].restorable);
    // A Trash tab is given them, as it is given the bin's runs.
    let mut trash = crate::trash::TrashView::new();
    trash.set_bots(rows);
    trash.catch_up(&w.ws);
    let node = koru_core::NodeId::from_uuid(chat.as_uuid());
    let (bots, rest) = trash.split_bots(vec![node]);
    assert_eq!(bots, [crate::bots::trash::BotSubject::Chat { bot, chat }]);
    assert!(rest.is_empty());
    let mut tab = Tab::trash();
    tab.view = TabView::Trash(trash);
    v.tabs_mut().push(tab);
    // A permanent delete of a Bot row asks nothing and does nothing.
    v.pending_delete_forever = Some((vec![node], false));
    v.drain_trash_all(&mut w.ws);
    assert!(v.confirm.is_none(), "no question about what is not built");
    // Emptying the bin says what it leaves.
    let doc = a_document(&mut w.ws, "A");
    v.pending_trash = Some((doc, false));
    v.drain_trash_all(&mut w.ws);
    v.pending_empty_trash = Some(false);
    v.drain_trash_all(&mut w.ws);
    let (dialog, _) = v.confirm.as_ref().expect("it asked");
    assert!(
        dialog.body().contains("1 item will be removed"),
        "{}",
        dialog.body()
    );
    assert!(
        dialog
            .body()
            .contains("Bots and bot conversations in the Trash stay there"),
        "{}",
        dialog.body()
    );
}
