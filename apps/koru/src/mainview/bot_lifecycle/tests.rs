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
    let (commands, _answers, _) = push_bot_tab(&mut v, &bot);
    // The answers' end stays open for the test's life.
    std::mem::forget(_answers);
    (v, commands, bot)
}

/// Push one more tab of `bot`, in front, over its own link: the commands
/// it sends, the end its answers go in by, and the tab's identity.
fn push_bot_tab(
    v: &mut MainView,
    bot: &Bot,
) -> (
    Receiver<Command>,
    std::sync::mpsc::SyncSender<crate::bots::link::Answer>,
    u64,
) {
    let (link, commands, answers) = BotsLink::test_pair();
    let mut view = BotView::with_link(bot.id, bot.main_chat, link);
    koru_workspace_host::View::set_viewport(&mut view, 800.0, 600.0);
    view.know_bot_for_test(bot.clone());
    let tab = Tab::bot(view);
    let id = tab.id;
    v.tabs_mut().push(tab);
    (commands, answers, id)
}

/// The identity of the tab in front.
fn front(v: &mut MainView) -> u64 {
    v.tabs_mut().active_mut().id
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
    let tab = front(&mut v);
    assert!(v.choose_bot_lifecycle(bot.id, tab, MenuAction::ArchiveBot));
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
            tab,
            ask: Ask::Archive
        }
    );
    assert!(writes(&commands).is_empty(), "nothing before the answer");
    v.settle_confirm(false);
    assert!(v.confirm.is_none());
    assert!(writes(&commands).is_empty(), "a no sends nothing");

    v.choose_bot_lifecycle(bot.id, tab, MenuAction::TrashBot);
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
    let tab = front(&mut v);
    let TabView::Bot(view) = &mut v.tabs_mut().active_mut().view else {
        panic!("a Bot tab");
    };
    assert_eq!(view.lifecycle_rows(), [Ask::Archive, Ask::Trash]);
    let chat = bot.main_chat;
    assert!(view.trash_conversation(chat));
    let menu = crate::mainview::context_menu::ContextMenu::new(
        &mut v.engine,
        MenuSubject::BotConversation {
            bot: bot.id,
            chat,
            tab,
        },
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

/// **A question is answered to the tab that asked** — by its identity, not
/// to whichever of the Bot's tabs is in front when the answer comes. Two
/// tabs of one Bot: the second asks, the first comes to the front while the
/// question is up, and the yes is sent from the second.
#[test]
fn a_question_is_answered_to_the_tab_that_asked() {
    let (mut v, _w) = container();
    let bot = crate::bots::tests::bot(BotId::mint(), WorkspaceId::new(), "Scout", false);
    let (first, _first_answers, _) = push_bot_tab(&mut v, &bot);
    let first_at = v.tabs_mut().active();
    let (second, _second_answers, asking) = push_bot_tab(&mut v, &bot);
    assert!(v.choose_bot_lifecycle(bot.id, asking, MenuAction::ArchiveBot));
    assert!(
        v.tabs_mut().select(first_at),
        "the other tab comes in front"
    );
    v.settle_confirm(true);
    assert!(writes(&first).is_empty(), "never the tab in front");
    let sent = writes(&second);
    assert!(
        matches!(sent.as_slice(), [BotCommand::Archive { bot_id, .. }] if *bot_id == bot.id),
        "{sent:?}"
    );
    // A tab that closed meanwhile is asked nothing, and nothing is sent.
    assert!(!v.choose_bot_lifecycle(bot.id, u64::MAX, MenuAction::TrashBot));
}

/// **A Restore from the Trash tells every tab of the Bot**, not only one:
/// each says what it holds now rather than what it held. The Restore is
/// answered as `restore` answers it (`apps/koru-ai-server/src/worker/bots/
/// lifecycle.rs`): its progress, completed.
#[test]
fn a_restore_from_the_trash_tells_every_tab_of_the_bot() {
    let (mut v, _w) = container();
    let home = WorkspaceId::new();
    let mut bot = crate::bots::tests::bot(BotId::mint(), home, "Scout", true);
    let mut tabs = Vec::new();
    for _ in 0..2 {
        let (commands, answers, id) = push_bot_tab(&mut v, &bot);
        tabs.push((commands, answers, id));
    }
    // Each tab shows a refusal: an Archive the owner refused.
    for (commands, answers, id) in &tabs {
        let view = bot_view_by_id(&mut v, *id);
        assert!(view.begin_lifecycle(Ask::Archive));
        let request = commands
            .try_iter()
            .find_map(|command| match command {
                Command::Write { request, .. } => Some(request),
                _ => None,
            })
            .expect("sent");
        answers
            .send(crate::bots::link::Answer::Wrote {
                bot: bot.id,
                request,
                result: Err(crate::bots::link::Failure::Refused(
                    "revision_conflict".into(),
                )),
            })
            .unwrap();
        view.shown_pass(None);
        assert!(view.lifecycle_said().is_some_and(|said| said.refused));
    }
    // The Trash restores the Bot.
    let (link, trash_commands, trash_answers) = BotsLink::test_pair();
    v.bot_trash = crate::bots::trash::BotTrash::with_link(link);
    v.maintain_bot_trash();
    let source = BotRequestId::mint();
    bot.lifecycle = koru_ai_core::bots::BotLifecycle::Trashed;
    bot.held_by = Some(koru_ai_core::bots::BotLifecycleHolder {
        request_id: source,
        kind: BotLifecycleKind::Trash,
        completed_at_ms: 1,
    });
    v.bot_trash.listed_for_test(vec![bot.clone()], Vec::new());
    assert!(
        v.bot_trash
            .restore(crate::bots::trash::BotSubject::Bot(bot.id))
    );
    let request = trash_commands
        .try_iter()
        .find_map(|command| match command {
            Command::Write { request, .. } => Some(request),
            _ => None,
        })
        .expect("the Restore");
    trash_answers
        .send(crate::bots::link::Answer::Wrote {
            bot: bot.id,
            request,
            result: Ok(koru_ai_core::bots::BotResponse::Lifecycle {
                operation: koru_ai_core::bots::BotLifecycleProgress {
                    request_id: request,
                    bot_id: bot.id,
                    kind: BotLifecycleKind::Restore,
                    chat_id: None,
                    source_request_id: Some(source),
                    stage: koru_ai_core::bots::BotLifecycleStage::Completed,
                    refusal: None,
                    members: Vec::new(),
                    left_in_place: Vec::new(),
                },
            }),
        })
        .unwrap();
    v.maintain_bot_trash();
    for (_, _, id) in &tabs {
        let view = bot_view_by_id(&mut v, *id);
        assert!(
            !view.lifecycle_said().is_some_and(|said| said.refused),
            "tab {id} was told: its old refusal is gone"
        );
    }
}

/// The Bot tab with identity `id`.
fn bot_view_by_id(v: &mut MainView, id: u64) -> &mut BotView {
    let at = v
        .tabs_mut()
        .position(|tab| tab.id == id)
        .expect("the tab is open");
    match &mut v.tabs_mut().get_mut(at).expect("open").view {
        TabView::Bot(view) => view,
        _ => panic!("a Bot tab"),
    }
}

/// **A link's receipt offers Restore only where one press restores.** With
/// nothing listed yet it says only that the conversation is in the Trash,
/// and reads the owner's listing; the answer decides: a conversation that
/// went with its Bot's Trash is restored by the Bot's Restore (one press);
/// one trashed on its own before its Bot went too says the Bot comes back
/// first, and offers nothing. Answered as `list_trashed_bot_chats` answers,
/// `bot_held_by` included (`crates/koru-ai-sqlite/src/repository/bots/
/// reads.rs`).
#[test]
fn a_links_receipt_offers_restore_only_where_one_press_restores() {
    use crate::bots::link::{Answer, Listing};
    use koru_ai_core::bots::{BotChatRestore, BotLifecycleHolder, BotLifecycleKind as Kind};
    let (mut v, _w) = container();
    let (link, commands, answers) = BotsLink::test_pair();
    v.bot_trash = crate::bots::trash::BotTrash::with_link(link);
    v.maintain_bot_trash();
    let home = WorkspaceId::new();
    let trash = BotRequestId::mint();
    let mut bot = crate::bots::tests::bot(BotId::mint(), home, "Writer", true);
    bot.lifecycle = koru_ai_core::bots::BotLifecycle::Trashed;
    bot.held_by = Some(BotLifecycleHolder {
        request_id: trash,
        kind: Kind::Trash,
        completed_at_ms: 1,
    });
    let (with_bot, on_its_own) = (koru_ai_ui::ChatId::new(), koru_ai_ui::ChatId::new());
    let mut went_with = crate::bots::tests::trashed_chat(
        bot.id,
        "Writer",
        with_bot,
        "Notes",
        (trash, Kind::Trash),
        BotChatRestore::Bot,
    );
    went_with.bot_held_by = bot.held_by.clone();
    let mut own = crate::bots::tests::trashed_chat(
        bot.id,
        "Writer",
        on_its_own,
        "Drafts",
        (BotRequestId::mint(), Kind::TrashChat),
        BotChatRestore::Bot,
    );
    own.bot_held_by = bot.held_by.clone();

    v.say_link_in_trash(with_bot);
    let (toast, payload) = v.toast.as_ref().expect("said at once");
    assert_eq!(toast.message(), "That conversation is in the Trash.");
    assert_eq!(toast.action(), None, "nothing offered before the listing");
    assert_eq!(
        *payload,
        crate::mainview::ToastPayload::BotChatInTrash { chat: with_bot }
    );
    // The owner's two listings answer.
    for command in commands.try_iter() {
        match command {
            Command::Lifecycle {
                seq,
                workspace,
                lifecycle,
            } => answers
                .send(Answer::Lifecycle {
                    seq,
                    workspace,
                    lifecycle,
                    result: Ok(vec![bot.clone()]),
                })
                .unwrap(),
            Command::TrashedChats { seq, workspace } => answers
                .send(Answer::TrashedChats {
                    seq,
                    workspace,
                    result: Ok(Listing::all(vec![went_with.clone(), own.clone()])),
                })
                .unwrap(),
            _ => panic!("the Trash's two reads"),
        }
    }
    v.maintain_bot_trash();
    let (toast, payload) = v.toast.as_ref().expect("said again");
    assert_eq!(
        toast.message(),
        "That conversation went to the Trash with its bot. Restoring the bot brings it back."
    );
    assert_eq!(toast.action(), Some("Restore"));
    assert_eq!(
        *payload,
        crate::mainview::ToastPayload::RestoreBotChat {
            bot: bot.id,
            chat: with_bot
        }
    );
    // Listed now: the other is decided at once, and offers nothing.
    v.say_link_in_trash(on_its_own);
    let (toast, _) = v.toast.as_ref().unwrap();
    assert_eq!(toast.action(), None);
    assert!(
        toast
            .message()
            .contains("Restore the bot from the Trash first"),
        "{}",
        toast.message()
    );
    assert!(commands.try_recv().is_err(), "nothing more was read");
}

/// **Empty Trash with only Bot rows says a Bot cannot be deleted for good
/// yet** — never a silent no-op — and over documents and Bot rows the
/// question keeps the Bots' sentence a paragraph of its own, since the
/// English-only body before it cannot carry another language's sentence.
#[test]
fn empty_trash_with_only_bot_rows_says_bots_cannot_be_deleted_yet() {
    let (mut v, mut w) = container();
    v.bot_trash.set_workspace(Some(WorkspaceId::new()));
    v.bot_trash.listed_for_test(
        Vec::new(),
        vec![crate::bots::tests::trashed_chat(
            BotId::mint(),
            "Writer",
            koru_ai_ui::ChatId::new(),
            "Notes",
            (BotRequestId::mint(), BotLifecycleKind::TrashChat),
            koru_ai_core::bots::BotChatRestore::Conversation,
        )],
    );
    v.pending_empty_trash = Some(false);
    v.drain_trash_all(&mut w.ws);
    assert!(v.confirm.is_none(), "nothing to ask about");
    let (toast, _) = v.toast.as_ref().expect("said");
    assert_eq!(
        toast.message(),
        koru_i18n::t(koru_i18n::Lang::En, koru_i18n::Key::TrashEmptyOnlyBots)
    );
    let doc = a_document(&mut w.ws, "A");
    v.pending_trash = Some((doc, false));
    v.drain_trash_all(&mut w.ws);
    v.pending_empty_trash = Some(false);
    v.drain_trash_all(&mut w.ws);
    let (dialog, _) = v.confirm.as_ref().expect("it asked");
    let note = koru_i18n::t(koru_i18n::Lang::En, koru_i18n::Key::TrashEmptyBotsNote);
    assert!(
        dialog.body().ends_with(&format!("\n\n{note}")),
        "{}",
        dialog.body()
    );
}
