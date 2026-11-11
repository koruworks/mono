// SPDX-License-Identifier: MPL-2.0

//! The window's half of a Bot's lifecycle (S14): its menu and its question,
//! nothing sent before a yes, the keyboard's return, and the Trash's Bot
//! rows — Restore only where it can be asked for, never a permanent delete,
//! and an Empty Trash that says what it leaves.

use std::sync::mpsc::Receiver;

use koru_ai_core::bots::{Bot, BotCommand, BotId, BotLifecycleKind, BotRequestId};
use koru_i18n::{Key as Words, t};
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

/// The whole window's published tree is one AccessKit can take
/// (`crate::bots::tests::assert_publishable`): the Bot tab, the roster,
/// the Trash and any question, together.
fn window_publishable(v: &mut MainView, stage: &str) {
    v.seat_accessibility(koru_workspace_host::accessibility::WindowSlot::from_raw(1));
    let tree = v.semantics(
        koru_workspace_host::accessibility::ViewSlot::from_raw(1),
        koru_workspace_host::accessibility::Scope::Whole,
    );
    crate::bots::tests::assert_publishable(stage, &tree);
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
    window_publishable(&mut v, "a Bot tab in front");
    assert!(v.choose_bot_lifecycle(bot.id, tab, MenuAction::ArchiveBot));
    window_publishable(&mut v, "the Archive question");
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

/// **Archive and Move to Trash dim, not hide, while a change of the Bot is
/// still being delivered** — the owner refuses both with
/// `bot_operation_pending` until what runs the Bot takes it — and are
/// offered again once the Bot's read names nothing. The read is as `read`
/// fills it in (`bot_pending_change`,
/// `apps/koru-ai-server/src/worker/bots/admission.rs`).
#[test]
fn archive_and_trash_dim_while_a_change_is_delivered() {
    let (mut v, _commands, mut bot) = with_bot_tab();
    let tab = front(&mut v);
    let id = bot.id;
    let menu = |v: &mut MainView| {
        crate::mainview::context_menu::ContextMenu::new(
            &mut v.engine,
            MenuSubject::Bot { bot: id, tab },
            (10.0, 10.0),
            vec![MenuAction::ArchiveBot, MenuAction::TrashBot],
        )
    };
    let offered = menu(&mut v);
    assert_eq!(v.enabled_items(&offered), [true, true]);
    bot.pending_change = Some(koru_ai_core::bots::BotPendingChange {
        request_id: BotRequestId::mint(),
        kind: koru_ai_core::bots::BotOperationKind::Pause,
        since_ms: 1,
        last_answer: None,
    });
    let TabView::Bot(view) = &mut v.tabs_mut().active_mut().view else {
        panic!("a Bot tab");
    };
    view.know_bot_for_test(bot.clone());
    let held = menu(&mut v);
    assert_eq!(
        v.enabled_items(&held),
        [false, false],
        "dimmed, still listed"
    );
    bot.pending_change = None;
    let TabView::Bot(view) = &mut v.tabs_mut().active_mut().view else {
        panic!("a Bot tab");
    };
    view.know_bot_for_test(bot);
    let again = menu(&mut v);
    assert_eq!(v.enabled_items(&again), [true, true]);
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
    window_publishable(&mut v, "the Trash with a Bot row");
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
    // A tab that closed meanwhile is asked nothing, and nothing is sent —
    // but that is said, never dropped in silence (the second review's
    // item 12).
    v.toast = None;
    assert!(v.choose_bot_lifecycle(bot.id, u64::MAX, MenuAction::TrashBot));
    assert!(writes(&first).is_empty() && writes(&second).is_empty());
    let said = |v: &MainView| {
        v.toast
            .as_ref()
            .map(|(toast, _)| toast.message().to_owned())
    };
    let gone = t(crate::format::lang(), Words::BotsAskingTabGone).to_owned();
    assert_eq!(said(&v), Some(gone.clone()));
    // A question answered after its tab closed: the same.
    v.toast = None;
    v.settle_bot_confirm(
        &ConfirmSubject::BotLifecycle {
            bot: bot.id,
            tab: u64::MAX,
            ask: Ask::Trash,
        },
        true,
    );
    assert_eq!(said(&v), Some(gone));
    assert!(writes(&first).is_empty() && writes(&second).is_empty());
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

// ------------------- Cancel is the default when a reply would be stopped

/// What the window knows of running work, case by case.
#[derive(Copy, Clone, Debug, PartialEq)]
enum Known {
    Running,
    NeedsYou,
    Idle,
    Unknown,
}

/// The three questions.
#[derive(Copy, Clone, Debug, PartialEq)]
enum Question {
    Archive,
    Trash,
    TrashConversation,
}

/// Ask `question` with the roster's activity read as `known` says: an
/// entry for `chat` running or waiting, as `list_activity` answers
/// (`apps/koru-ai-server/src/worker/bots/reads.rs`), none, or never read.
fn ask_with(question: Question, known: Known) -> (MainView, Receiver<Command>, Bot, u64) {
    let (mut v, commands, bot) = with_bot_tab();
    let tab = front(&mut v);
    // A whole Bot's question counts any of its conversations — here not the
    // one its tab shows; a conversation's, that conversation alone.
    let chat = match question {
        Question::TrashConversation => bot.main_chat,
        Question::Archive | Question::Trash => koru_ai_ui::ChatId::new(),
    };
    let entries = |state| {
        vec![koru_ai_core::bots::BotActivity {
            bot_id: bot.id,
            chat_id: chat,
            state,
        }]
    };
    let read = match known {
        Known::Running => Some(entries(koru_ai_core::bots::BotActivityState::Running)),
        Known::NeedsYou => Some(entries(koru_ai_core::bots::BotActivityState::NeedsYou)),
        Known::Idle => Some(Vec::new()),
        Known::Unknown => None,
    };
    if let Some(read) = read {
        v.explore
            .bots_mut()
            .activity_read_for_test(read, false, std::time::Instant::now());
    }
    let asked = match question {
        Question::Archive => v.choose_bot_lifecycle(bot.id, tab, MenuAction::ArchiveBot),
        Question::Trash => v.choose_bot_lifecycle(bot.id, tab, MenuAction::TrashBot),
        Question::TrashConversation => v.ask_trash_bot_conversation(bot.id, tab, bot.main_chat),
    };
    assert!(asked, "{question:?} {known:?}: it asks");
    (v, commands, bot, tab)
}

/// The window's published tree while its question stands: the dialog.
fn dialog_node(v: &MainView) -> koru_workspace_host::accessibility::SemanticNode {
    let tree = v.semantics(
        koru_workspace_host::accessibility::ViewSlot::from_raw(1),
        koru_workspace_host::accessibility::Scope::Whole,
    );
    fn find(
        nodes: &[koru_workspace_host::accessibility::SemanticNode],
    ) -> Option<koru_workspace_host::accessibility::SemanticNode> {
        nodes.iter().find_map(|node| {
            (node.role == accesskit::Role::Dialog)
                .then(|| node.clone())
                .or_else(|| find(&node.children))
        })
    }
    find(&tree).expect("the question is published")
}

/// **Cancel is the default when an Archive, a Trash or a conversation's
/// Trash will stop work running now** (QUESTIONS O46, the owner's): a reply
/// running or waiting on the person — in any of the Bot's conversations for
/// the whole Bot, in that conversation for its Trash — or work the window
/// cannot rule out (the activity never read). The question then says a
/// reply will be stopped when it knows one is. Only a known idle keeps
/// Enter confirming, with the words as they always were. For every case:
/// Enter presses the default, Escape cancels, Tab reaches both answers, and
/// the published tree marks the default — and only it — as the default
/// button.
#[test]
fn cancel_is_the_default_when_the_question_would_stop_a_reply_running_now() {
    use koru_ui::confirm::Outcome;
    use winit::keyboard::NamedKey;
    let lang = crate::format::lang();
    for question in [
        Question::Archive,
        Question::Trash,
        Question::TrashConversation,
    ] {
        for known in [Known::Running, Known::NeedsYou, Known::Idle, Known::Unknown] {
            let case = format!("{question:?} {known:?}");
            let (mut v, commands, _bot, _tab) = ask_with(question, known);
            let conversation = question == Question::TrashConversation;
            let (dialog, _) = v.confirm.as_ref().expect("it asks");
            let default = if known == Known::Idle {
                Outcome::Confirm
            } else {
                Outcome::Cancel
            };
            assert_eq!(dialog.default_action(), Some(default), "{case}");
            let lead = match (known, conversation) {
                (Known::Running, false) => Some(Words::BotsStopsRunning),
                (Known::NeedsYou, false) => Some(Words::BotsStopsWaiting),
                (Known::Running, true) => Some(Words::BotsConversationStopsRunning),
                (Known::NeedsYou, true) => Some(Words::BotsConversationStopsWaiting),
                _ => None,
            };
            let leads = [
                Words::BotsStopsRunning,
                Words::BotsStopsWaiting,
                Words::BotsConversationStopsRunning,
                Words::BotsConversationStopsWaiting,
            ];
            match lead {
                Some(words) => assert!(
                    dialog
                        .body()
                        .starts_with(&format!("{}\n\n", t(lang, words))),
                    "{case}: {}",
                    dialog.body()
                ),
                None => assert!(
                    leads
                        .iter()
                        .all(|words| !dialog.body().contains(t(lang, *words))),
                    "{case}: nothing said to be running: {}",
                    dialog.body()
                ),
            }
            assert!(
                dialog
                    .body()
                    .contains("including a session you have taken over"),
                "{case}: what always happens is still said"
            );
            // The tree marks the default answer, and only it.
            v.seat_accessibility(koru_workspace_host::accessibility::WindowSlot::from_raw(1));
            let node = dialog_node(&v);
            assert_eq!(
                node.label.as_deref(),
                Some(dialog_title(&v).as_str()),
                "{case}"
            );
            let defaults: Vec<_> = node
                .children
                .iter()
                .filter(|child| child.role == accesskit::Role::DefaultButton)
                .filter_map(|child| child.label.clone())
                .collect();
            let cancel = t(lang, Words::BotsDialogCancel).to_owned();
            let expected = if default == Outcome::Cancel {
                cancel.clone()
            } else {
                dialog_verb(question, lang)
            };
            assert_eq!(defaults, [expected], "{case}");
            let answers = node
                .children
                .iter()
                .filter(|child| {
                    matches!(
                        child.role,
                        accesskit::Role::Button | accesskit::Role::DefaultButton
                    )
                })
                .count();
            assert_eq!(answers, 2, "{case}: both answers published");
            // The question's words are text in the dialog, first, and its
            // description — never only drawn.
            let body = dialog_body(&v);
            assert_eq!(node.description.as_deref(), Some(body.as_str()), "{case}");
            assert_eq!(
                node.children
                    .first()
                    .map(|child| (child.role, child.label.clone())),
                Some((accesskit::Role::Label, Some(body))),
                "{case}: the body is published as text"
            );
            // What is drawn ringed is what Enter presses.
            let (dialog, _) = v.confirm.as_ref().unwrap();
            assert_eq!(dialog.focused(), Some(default), "{case}: the ring");
            // The keyboard, **as a real key enters**: the window's own
            // `View::key`, which the shell calls for every press
            // (`koru_workspace_host::app`, `WindowEvent::KeyboardInput`).
            // Enter presses the ringed default, so a Cancel sends nothing
            // and a yes sends.
            let mut ws =
                koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t")
                    .expect("a document");
            assert!(press(&mut v, &mut ws, NamedKey::Enter), "{case}");
            assert!(v.confirm.is_none(), "{case}: answered");
            let sent = writes(&commands);
            assert_eq!(
                sent.is_empty(),
                default == Outcome::Cancel,
                "{case}: {sent:?}"
            );
            // Escape cancels, whatever the default.
            let (mut v, commands, _bot, _tab) = ask_with(question, known);
            assert!(press(&mut v, &mut ws, NamedKey::Escape));
            assert!(
                v.confirm.is_none() && writes(&commands).is_empty(),
                "{case}"
            );
            // Tab moves the ring from the default to the other answer and
            // back, and Enter follows the ring.
            let (mut v, commands, _bot, _tab) = ask_with(question, known);
            let other = if default == Outcome::Cancel {
                Outcome::Confirm
            } else {
                Outcome::Cancel
            };
            for (step, ringed) in [(1, other), (2, default)] {
                assert!(press(&mut v, &mut ws, NamedKey::Tab));
                let (dialog, _) = v.confirm.as_ref().unwrap();
                assert_eq!(dialog.focused(), Some(ringed), "{case}: Tab {step}");
                assert_eq!(dialog.default_action(), Some(ringed), "{case}: Tab {step}");
            }
            assert!(press(&mut v, &mut ws, NamedKey::Tab));
            assert!(press(&mut v, &mut ws, NamedKey::Enter));
            assert_eq!(
                writes(&commands).is_empty(),
                other == Outcome::Cancel,
                "{case}: Enter on the ringed other answer"
            );
        }
    }
}

/// A key pressed on the window as the shell hands it over.
fn press(
    v: &mut MainView,
    ws: &mut koru_core::schema::WorkspaceDoc,
    key: winit::keyboard::NamedKey,
) -> bool {
    koru_workspace_host::View::key(
        v,
        ws,
        &winit::keyboard::Key::Named(key),
        None,
        winit::keyboard::ModifiersState::empty(),
    )
}

/// The question's title, as asked.
fn dialog_title(v: &MainView) -> String {
    v.confirm
        .as_ref()
        .map(|(dialog, _)| dialog.title().to_owned())
        .unwrap_or_default()
}

/// The question's body, as asked.
fn dialog_body(v: &MainView) -> String {
    v.confirm
        .as_ref()
        .map(|(dialog, _)| dialog.body().to_owned())
        .unwrap_or_default()
}

/// The answering button's word for `question`.
fn dialog_verb(question: Question, lang: koru_i18n::Lang) -> String {
    t(
        lang,
        match question {
            Question::Archive => Words::BotsArchiveConfirm,
            Question::Trash | Question::TrashConversation => Words::BotsTrashConfirm,
        },
    )
    .to_owned()
}

/// **The tab's own conversation is evidence of work running**: with no
/// activity read, a reply the tab shows running makes Cancel the default
/// and says so. A click on the published default answer cancels, as a
/// press would, and sends nothing; a click on the other confirms.
#[test]
fn the_tabs_own_running_reply_makes_cancel_the_default_and_a_click_answers() {
    use koru_ui::confirm::Outcome;
    let (mut v, commands, bot) = with_bot_tab();
    let tab = front(&mut v);
    let TabView::Bot(view) = &mut v.tabs_mut().active_mut().view else {
        panic!("a Bot tab");
    };
    view.conversation_mut()
        .expect("bound")
        .set_status(koru_chats_ui::ChatStatus::Working, false);
    assert!(v.choose_bot_lifecycle(bot.id, tab, MenuAction::ArchiveBot));
    let (dialog, _) = v.confirm.as_ref().expect("it asks");
    assert_eq!(dialog.default_action(), Some(Outcome::Cancel));
    let lang = crate::format::lang();
    assert!(dialog.body().starts_with(t(lang, Words::BotsStopsRunning)));
    v.seat_accessibility(koru_workspace_host::accessibility::WindowSlot::from_raw(1));
    let node = dialog_node(&v);
    let mut ws = koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t")
        .expect("a document");
    let default = node
        .children
        .iter()
        .find(|child| child.role == accesskit::Role::DefaultButton)
        .expect("a default")
        .occurrence;
    assert!(v.accessibility_action(&mut ws, default, accesskit::Action::Click, None));
    assert!(v.confirm.is_none(), "answered");
    assert!(writes(&commands).is_empty(), "Cancel sends nothing");
    assert!(v.choose_bot_lifecycle(bot.id, tab, MenuAction::ArchiveBot));
    let other = dialog_node(&v)
        .children
        .iter()
        .find(|child| child.role == accesskit::Role::Button)
        .expect("the other answer")
        .occurrence;
    assert!(v.accessibility_action(&mut ws, other, accesskit::Action::Click, None));
    let sent = writes(&commands);
    assert!(
        matches!(sent.as_slice(), [BotCommand::Archive { bot_id, .. }] if *bot_id == bot.id),
        "{sent:?}"
    );
}
