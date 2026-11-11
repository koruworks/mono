// SPDX-License-Identifier: MPL-2.0

//! **A Bot tab's lifecycle** (Bots P1.8 S14), driven through the tab with
//! fake owner replies: nothing is sent before the window's confirmation;
//! progress, parked, Continue and Cancel; the tab after Archive, Unarchive,
//! Trash and Restore; one conversation's Trash; the keyboard's return.

use super::*;
use crate::bots::lifecycle::Ask;
use koru_ai_core::bots::{
    BotChatPurpose, BotLifecycleKind, BotLifecycleProgress, BotLifecycleRefusal, BotLifecycleStage,
};

fn progress(
    bot: &Bot,
    request: BotRequestId,
    kind: BotLifecycleKind,
    stage: BotLifecycleStage,
) -> Result<BotResponse, Failure> {
    Ok(BotResponse::Lifecycle {
        operation: BotLifecycleProgress {
            request_id: request,
            bot_id: bot.id,
            kind,
            chat_id: None,
            source_request_id: None,
            stage,
            refusal: (stage == BotLifecycleStage::Incomplete)
                .then_some(BotLifecycleRefusal::WorkRunning),
            members: Vec::new(),
            left_in_place: Vec::new(),
        },
    })
}

/// Nothing at all with the link.
fn nothing_sent(f: &Fixture) {
    let sent: Vec<_> = f
        .commands
        .try_iter()
        .filter(|command| !matches!(command, Command::ListChats { .. }))
        .collect();
    assert!(sent.is_empty(), "nothing was sent");
}

/// **The ⋯ offers Archive and Move to Trash, and pressing it sends
/// nothing**: it asks the window for its menu. Each confirmation says what
/// will happen in the person's words — running work stopped, a session they
/// took over included; for Trash, the conversations go and the role page,
/// results and folder stay.
#[test]
fn the_menu_and_its_questions_send_nothing_and_say_what_will_happen() {
    let mut f = listed(&[]);
    assert_eq!(f.view.lifecycle_rows(), [Ask::Archive, Ask::Trash]);
    assert!(f.view.perform(region::Act::Actions));
    let at = f
        .view
        .take_actions_request()
        .expect("the menu is the window's");
    assert!(at.0 > 0.0 && at.1 > 0.0, "under the ⋯: {at:?}");
    let (title, body, verb) = f.view.confirm_words(Ask::Archive).unwrap();
    assert_eq!(title, "Archive “Scout”?");
    assert!(
        body.contains("including a session you have taken over"),
        "{body}"
    );
    assert_eq!(verb, "Archive");
    let (title, body, verb) = f.view.confirm_words(Ask::Trash).unwrap();
    assert_eq!(title, "Move “Scout” to Trash?");
    assert!(body.contains("including a session you have taken over"));
    assert!(body.contains("conversations go to the Trash with it"));
    assert!(
        body.contains("role page, results and working folder stay where they are"),
        "never that the role page goes to Trash: {body}"
    );
    assert!(!body.contains("role page goes"), "{body}");
    assert_eq!(verb, "Move to Trash");
    nothing_sent(&f);
    // Every language has its own words for each.
    for lang in Lang::ALL {
        for key in [
            Words::BotsArchiveBody,
            Words::BotsTrashBody,
            Words::BotsTrashConversationBody,
            Words::BotsTrashMainBody,
            Words::BotsSettling,
            Words::BotsLifecycleIncomplete,
            Words::BotsInTrash,
        ] {
            assert!(!t(lang, key).trim().is_empty(), "{lang:?} {key:?}");
        }
    }
}

/// **Progress is the owner's**: *Finishing the bot's current work…* while
/// it holds the Archive as moving; parked, the sentence with Continue and
/// Cancel, and only read meanwhile; Continue sends the same command by its
/// request, Cancel the Restore naming it.
#[test]
fn progress_parked_continue_and_cancel() {
    let mut f = listed(&[]);
    assert!(f.view.begin_lifecycle(Ask::Archive));
    let (request, command) = f.wrote();
    assert!(matches!(
        command,
        BotCommand::Archive { request_id, expected_revision, .. }
            if request_id == request && expected_revision == f.bot.revision
    ));
    assert!(
        f.view.lifecycle_rows().is_empty(),
        "no second ask meanwhile"
    );
    let bot = f.bot.clone();
    f.reply(
        request,
        progress(
            &bot,
            request,
            BotLifecycleKind::Archive,
            BotLifecycleStage::Captured,
        ),
    );
    assert_eq!(
        f.view.lifecycle_said().map(|said| said.words),
        Some(Words::BotsSettling)
    );
    // Read again while shown: a ReadLifecycleOperation, by its request.
    f.view.lifecycle_read_now();
    f.view.shown_pass(None);
    let (asked, read) = f.wrote();
    assert_eq!(asked, request);
    assert!(matches!(
        read,
        BotCommand::ReadLifecycleOperation { request_id, .. } if request_id == request
    ));
    f.reply(
        request,
        progress(
            &bot,
            request,
            BotLifecycleKind::Archive,
            BotLifecycleStage::Incomplete,
        ),
    );
    assert_eq!(
        f.view.lifecycle_said().map(|said| said.words),
        Some(Words::BotsLifecycleIncomplete)
    );
    let keys = f.keys();
    assert!(keys.contains(&actions::CONTINUE) && keys.contains(&actions::CANCEL_OPERATION));
    // Parked: never retried on its own — only read, on the slower clock,
    // since the owner may take it up again by itself.
    let now = std::time::Instant::now();
    let due = f.view.lifecycle_deadline(now).expect("read again later");
    assert!(
        due > now + std::time::Duration::from_secs(1),
        "the slower clock: {due:?}"
    );
    f.view.lifecycle_read_now();
    f.view.shown_pass(None);
    let writes: Vec<BotCommand> = f
        .commands
        .try_iter()
        .filter_map(|command| match command {
            Command::Write { command, .. } => Some(*command),
            _ => None,
        })
        .collect();
    assert!(
        matches!(
            writes.as_slice(),
            [BotCommand::ReadLifecycleOperation { request_id, .. }] if *request_id == request
        ),
        "a read, never the Archive again: {writes:?}"
    );
    f.reply(
        request,
        progress(
            &bot,
            request,
            BotLifecycleKind::Archive,
            BotLifecycleStage::Incomplete,
        ),
    );
    assert!(f.view.perform(region::Act::ContinueOperation));
    let (again, command) = f.wrote();
    assert_eq!(again, request);
    assert!(matches!(command, BotCommand::Archive { request_id, .. } if request_id == request));
    f.reply(
        request,
        progress(
            &bot,
            request,
            BotLifecycleKind::Archive,
            BotLifecycleStage::Incomplete,
        ),
    );
    assert!(f.view.perform(region::Act::CancelOperation));
    let (undo, command) = f.wrote();
    assert!(matches!(
        command,
        BotCommand::Restore { request_id, source_request_id, .. }
            if request_id == undo && source_request_id == request
    ));
}

/// **After Archive the tab says it is archived, its conversations stay
/// readable, and Unarchive is offered**; Unarchive brings it back paused.
#[test]
fn archive_then_unarchive_from_the_tab() {
    let mut f = listed(&[]);
    f.view.begin_lifecycle(Ask::Archive);
    let (request, _) = f.wrote();
    let bot = f.bot.clone();
    f.reply(
        request,
        progress(
            &bot,
            request,
            BotLifecycleKind::Archive,
            BotLifecycleStage::Completed,
        ),
    );
    let finished = f.view.take_lifecycle_finished();
    assert_eq!(finished.len(), 1, "the window's book is told");
    // The Bot is read again, and reads archived.
    f.view.shown_pass(None);
    let seq = f.asked();
    let mut archived = f.bot.clone();
    archived.lifecycle = BotLifecycle::Archived;
    archived.revision += 2;
    f.answer(seq, archived.id, Ok(archived.clone()));
    assert!(f.view.facts().unwrap().archived);
    assert!(f.view.shows_conversation(), "its history stays readable");
    assert!(f.keys().contains(&ARCHIVED));
    assert!(f.keys().contains(&actions::UNARCHIVE));
    assert_eq!(f.view.lifecycle_rows(), [Ask::Unarchive, Ask::Trash]);
    assert_eq!(
        f.view.conversations.surface.row_actions(),
        koru_chats_ui::RowActions::OPEN_ONLY,
        "an archived Bot's conversations are not trashed one by one"
    );
    assert!(f.view.perform(region::Act::Unarchive));
    let (undo, command) = f.wrote();
    assert!(matches!(
        command,
        BotCommand::Restore { source_request_id, expected_revision, .. }
            if source_request_id == request && expected_revision == archived.revision
    ));
    f.reply(
        undo,
        progress(
            &archived,
            undo,
            BotLifecycleKind::Restore,
            BotLifecycleStage::Completed,
        ),
    );
    assert_eq!(
        f.view.lifecycle_said().map(|said| said.words),
        Some(Words::BotsUnarchivedPaused)
    );
}

/// **After Trash the tab says it is in Trash with what stayed where it was,
/// shows nothing of the conversation, and offers Restore** at the revision
/// the Trash filter lists; Restore brings it back paused.
#[test]
fn trash_then_restore_from_the_tab() {
    let mut f = listed(&[]);
    f.view.begin_lifecycle(Ask::Trash);
    let (request, _) = f.wrote();
    let bot = f.bot.clone();
    f.reply(
        request,
        progress(
            &bot,
            request,
            BotLifecycleKind::Trash,
            BotLifecycleStage::Completed,
        ),
    );
    assert!(f.view.facts().unwrap().trashed);
    assert!(!f.view.shows_conversation());
    assert_eq!(
        f.view.lifecycle_said().map(|said| said.words),
        Some(Words::BotsInTrash)
    );
    assert!(t(Lang::En, Words::BotsInTrash).contains("role page, results and working folder"));
    assert!(
        !f.keys().contains(&region::SETTINGS_BUTTON),
        "a trashed Bot has no settings"
    );
    // Its revision is the Trash filter's to say.
    f.view.shown_pass(None);
    let (seq, home) = loop {
        match f.commands.try_recv().expect("the Trash filter is read") {
            Command::Lifecycle {
                seq,
                workspace,
                lifecycle: BotLifecycle::Trashed,
            } => break (seq, workspace),
            _ => continue,
        }
    };
    assert_eq!(home, f.bot.home_workspace);
    let mut trashed = f.bot.clone();
    trashed.lifecycle = BotLifecycle::Trashed;
    trashed.revision = 9;
    f.answers
        .send(Answer::Lifecycle {
            seq,
            workspace: home,
            lifecycle: BotLifecycle::Trashed,
            result: Ok(vec![trashed.clone()]),
        })
        .unwrap();
    f.view.poll();
    assert!(f.keys().contains(&actions::RESTORE));
    assert!(f.view.perform(region::Act::Restore));
    let (undo, command) = f.wrote();
    assert_eq!(
        command,
        BotCommand::Restore {
            request_id: undo,
            bot_id: f.bot.id,
            expected_revision: 9,
            source_request_id: request
        }
    );
    f.reply(
        undo,
        progress(
            &trashed,
            undo,
            BotLifecycleKind::Restore,
            BotLifecycleStage::Completed,
        ),
    );
    assert!(!f.view.facts().unwrap().trashed);
    assert_eq!(
        f.view.lifecycle_said().map(|said| said.words),
        Some(Words::BotsRestoredPaused)
    );
}

/// **One conversation's Trash**: a row's Delete asks the window, nothing is
/// sent before the answer, the row stays until the owner says it is done,
/// and the tab then moves off it to the main conversation.
#[test]
fn a_conversation_leaves_only_on_the_owners_word_and_the_tab_moves_off_it() {
    let other = ChatId::new();
    let mut f = listed(&[(other, BotChatPurpose::Additional)]);
    f.view.conversations.surface.ask_open(other);
    f.view.shown_pass(None);
    assert_eq!(f.view.chat_id(), other);
    f.view.conversations.surface.ask_delete(other);
    f.view.shown_pass(None);
    assert_eq!(f.view.take_conversation_trash(), Some(other));
    nothing_sent(&f);
    let (_, body, _) = f.view.conversation_trash_words(other).unwrap();
    assert!(body.contains("other conversations keep running"), "{body}");
    assert!(f.view.trash_conversation(other));
    let (request, command) = f.wrote();
    assert!(matches!(
        command,
        BotCommand::TrashChat { chat_id, .. } if chat_id == other
    ));
    assert_eq!(
        f.view.lifecycle_said().map(|said| said.words),
        Some(Words::BotsTrashingConversation)
    );
    assert_eq!(f.view.conversations.listed().len(), 2, "still listed");
    let bot = f.bot.clone();
    let mut done = progress(
        &bot,
        request,
        BotLifecycleKind::TrashChat,
        BotLifecycleStage::Completed,
    );
    if let Ok(BotResponse::Lifecycle { operation }) = &mut done {
        operation.chat_id = Some(other);
    }
    f.reply(request, done);
    assert_eq!(
        f.view.conversations.listed().len(),
        1,
        "left on the owner's word"
    );
    assert_eq!(f.view.chat_id(), f.bot.main_chat, "moved to the main one");
    assert!(f.view.shows_conversation());
}

/// **The main conversation to Trash** says the Bot keeps working without
/// it; with nothing else listed the tab says it is in Trash and offers *New
/// conversation*.
#[test]
fn the_main_conversation_in_trash_leaves_the_bot_working() {
    let mut f = listed(&[]);
    let main = f.bot.main_chat;
    let (_, body, _) = f.view.conversation_trash_words(main).unwrap();
    assert!(body.contains("keeps working without it"), "{body}");
    assert!(f.view.trash_conversation(main));
    let (request, _) = f.wrote();
    let bot = f.bot.clone();
    f.reply(
        request,
        progress(
            &bot,
            request,
            BotLifecycleKind::TrashChat,
            BotLifecycleStage::Completed,
        ),
    );
    assert!(!f.view.shows_conversation());
    assert_eq!(
        f.view.lifecycle_said().map(|said| said.words),
        Some(Words::BotsConversationInTrash)
    );
    assert!(f.keys().contains(&list::NEW_CONVERSATION));
    assert!(
        !f.view.facts().unwrap().trashed,
        "the Bot itself is not in Trash"
    );
}

/// **A second conversation's Trash waits** while one is on its way: said in
/// words, never queued.
#[test]
fn a_second_conversation_trash_waits_in_words() {
    let other = ChatId::new();
    let mut f = listed(&[(other, BotChatPurpose::Additional)]);
    assert!(f.view.trash_conversation(other));
    f.wrote();
    f.view.conversations.surface.ask_delete(f.bot.main_chat);
    f.view.shown_pass(None);
    assert_eq!(f.view.take_conversation_trash(), None);
    assert_eq!(f.view.list_notice(), Some(Words::BotsErrPending));
    assert!(f.keys().contains(&list::TRASH_NOTICE));
}

/// **A competing operation** (`request_conflict` today,
/// `bot_lifecycle_pending` after the engine's fix) says something else is
/// still finishing, and the Bot's read then shows that operation.
#[test]
fn a_competing_operation_is_said_and_then_shown() {
    for code in ["request_conflict", "bot_lifecycle_pending", "bot_trashed"] {
        let mut f = listed(&[]);
        f.view.begin_lifecycle(Ask::Trash);
        let (request, _) = f.wrote();
        f.reply(request, Err(Failure::Refused(code.into())));
        let said = f.view.lifecycle_said().expect("said");
        assert!(said.refused, "{code}");
        let expected = if code == "bot_trashed" {
            Words::BotsErrBotTrashed
        } else {
            Words::BotsErrPending
        };
        assert_eq!(said.words, expected, "{code}");
        f.view.shown_pass(None);
        let seq = f.asked();
        let mut held = f.bot.clone();
        held.open_operation = Some(koru_ai_core::bots::BotOpenLifecycle {
            request_id: BotRequestId::mint(),
            kind: BotLifecycleKind::TrashChat,
            chat_id: Some(ChatId::new()),
            stage: BotLifecycleStage::Incomplete,
            refusal: Some(BotLifecycleRefusal::WorkRunning),
        });
        f.answer(seq, held.id, Ok(held));
        assert!(f.keys().contains(&actions::CONTINUE), "{code}");
        assert!(f.view.lifecycle_busy(), "{code}");
    }
}

/// **A hidden tab reads nothing about its operation**; shown again, it reads
/// at once.
#[test]
fn a_hidden_tab_reads_no_progress() {
    let mut f = listed(&[]);
    f.view.begin_lifecycle(Ask::Archive);
    let (request, _) = f.wrote();
    let bot = f.bot.clone();
    f.reply(
        request,
        progress(
            &bot,
            request,
            BotLifecycleKind::Archive,
            BotLifecycleStage::Settling,
        ),
    );
    f.view.hidden();
    let mut ws = koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t")
        .expect("a document");
    let later = std::time::Instant::now() + std::time::Duration::from_secs(5);
    View::maintain(&mut f.view, &mut ws, later);
    assert!(
        f.commands
            .try_iter()
            .all(|command| !matches!(command, Command::Write { .. })),
        "hidden: no read"
    );
    f.view.shown_pass(None);
    let (asked, read) = f.wrote();
    assert_eq!(asked, request);
    assert!(matches!(read, BotCommand::ReadLifecycleOperation { .. }));
}

/// **The ⋯ is a named button before Settings in the keyboard's order and
/// the accessibility tree**, and the keyboard returns to it when the
/// window's dialog closes.
#[test]
fn the_actions_button_is_named_ordered_and_gets_the_keyboard_back() {
    let mut f = listed(&[]);
    let order = f.view.focus_order();
    let actions_at = order.iter().position(|key| *key == actions::ACTIONS_BUTTON);
    let settings_at = order.iter().position(|key| *key == region::SETTINGS_BUTTON);
    assert!(actions_at.unwrap() < settings_at.unwrap(), "{order:?}");
    let controls = f.view.controls();
    let named = controls
        .iter()
        .find(|control| control.key == actions::ACTIONS_BUTTON)
        .expect("published");
    assert_eq!(named.label, "Bot actions");
    assert_eq!(named.role, ControlRole::Button);
    // The keyboard opened the menu from the ⋯ …
    f.view.focus = Some(actions::ACTIONS_BUTTON);
    assert!(f.view.perform(region::Act::Actions));
    // … the window's menu and dialog took it …
    f.view.focus = None;
    // … and the dialog's close gives it back.
    f.view.dialog_closed();
    assert_eq!(f.view.focus(), Some(actions::ACTIONS_BUTTON));
    // A pointer-opened menu leaves the keyboard where it was.
    f.view.focus = None;
    f.view.perform(region::Act::Actions);
    f.view.dialog_closed();
    assert_eq!(f.view.focus(), None);
}

/// **Between an Archive's answer and the next read the tab offers only what
/// the owner would take**: Unarchive and Move to Trash, never Archive
/// again, and no conversation's Trash — the owner refuses that for an
/// archived Bot (`bot_archived`).
#[test]
fn after_an_archive_the_menu_and_rows_are_true_before_the_read() {
    let mut f = listed(&[]);
    f.view.begin_lifecycle(Ask::Archive);
    let (request, _) = f.wrote();
    let bot = f.bot.clone();
    f.reply(
        request,
        progress(
            &bot,
            request,
            BotLifecycleKind::Archive,
            BotLifecycleStage::Completed,
        ),
    );
    // No read of the Bot has answered yet.
    assert_eq!(f.view.lifecycle_rows(), [Ask::Unarchive, Ask::Trash]);
    assert_eq!(
        f.view.conversations.surface.row_actions(),
        koru_chats_ui::RowActions::OPEN_ONLY
    );
}

/// **A Restore gives the Bot back to what it was before its Trash** —
/// archived, trashed, restored comes back archived — so between the
/// Restore's answer and the Bot's read the tab neither claims it active
/// nor offers anything the owner might refuse; the read then says which.
#[test]
fn after_a_restore_nothing_is_claimed_until_the_read_says_what_the_bot_is() {
    let mut f = listed(&[]);
    // Archived first, as the owner's read says.
    f.view.shown_pass(None);
    let mut archived = f.bot.clone();
    archived.lifecycle = BotLifecycle::Archived;
    archived.held_by = Some(koru_ai_core::bots::BotLifecycleHolder {
        request_id: BotRequestId::mint(),
        kind: BotLifecycleKind::Archive,
        completed_at_ms: 1,
    });
    f.view.lifecycle_moved_elsewhere();
    f.view.shown_pass(None);
    let seq = bot_read(&f);
    f.answer(seq, archived.id, Ok(archived.clone()));
    assert!(f.view.facts().unwrap().archived);
    // Then into the Trash, and back.
    f.view.begin_lifecycle(Ask::Trash);
    let (trash, _) = f.wrote();
    f.reply(
        trash,
        progress(
            &archived,
            trash,
            BotLifecycleKind::Trash,
            BotLifecycleStage::Completed,
        ),
    );
    let mut trashed = archived.clone();
    trashed.lifecycle = BotLifecycle::Trashed;
    f.view.take_trash_listing(Ok(vec![trashed.clone()]));
    assert!(f.view.perform(region::Act::Restore));
    let (undo, _) = f.wrote();
    f.reply(
        undo,
        progress(
            &trashed,
            undo,
            BotLifecycleKind::Restore,
            BotLifecycleStage::Completed,
        ),
    );
    let facts = f.view.facts().unwrap();
    assert!(!facts.trashed);
    assert!(facts.archived, "never claimed active before the read");
    assert!(f.view.lifecycle_rows().is_empty(), "nothing offered yet");
    assert_eq!(
        f.view.conversations.surface.row_actions(),
        koru_chats_ui::RowActions::OPEN_ONLY
    );
    // The read says archived: what an archived Bot offers.
    f.view.shown_pass(None);
    let seq = bot_read(&f);
    f.answer(seq, archived.id, Ok(archived.clone()));
    assert!(f.view.facts().unwrap().archived);
    assert_eq!(f.view.lifecycle_rows(), [Ask::Unarchive, Ask::Trash]);
}

/// **A trashed Bot's conversation is let go** (verified: before this, the
/// tab kept its embedded chat bound and maintained behind the header, its
/// reader free to read a conversation the owner refuses as `bot_trashed`).
/// A Restore's read binds it again.
#[test]
fn a_trashed_bots_conversation_is_let_go_and_bound_again_on_restore() {
    let mut f = listed(&[]);
    assert!(f.view.conversation().is_some(), "bound while active");
    f.view.begin_lifecycle(Ask::Trash);
    let (request, _) = f.wrote();
    let bot = f.bot.clone();
    f.reply(
        request,
        progress(
            &bot,
            request,
            BotLifecycleKind::Trash,
            BotLifecycleStage::Completed,
        ),
    );
    assert!(f.view.conversation().is_none(), "nothing of it is bound");
    let mut trashed = f.bot.clone();
    trashed.lifecycle = BotLifecycle::Trashed;
    f.view.take_trash_listing(Ok(vec![trashed.clone()]));
    assert!(f.view.conversation().is_none());
    assert!(f.view.perform(region::Act::Restore));
    let (undo, _) = f.wrote();
    f.reply(
        undo,
        progress(
            &trashed,
            undo,
            BotLifecycleKind::Restore,
            BotLifecycleStage::Completed,
        ),
    );
    f.view.shown_pass(None);
    let seq = bot_read(&f);
    let active = f.bot.clone();
    f.answer(seq, active.id, Ok(active));
    assert!(f.view.conversation().is_some(), "bound again once read");
    assert_eq!(f.view.lifecycle_rows(), [Ask::Archive, Ask::Trash]);
}

/// **An Archive Koru AI does not answer** says Koru AI is not answering,
/// never *Finishing the bot's current work…*, while it is still asked
/// about.
#[test]
fn an_unanswered_archive_says_koru_ai_is_not_answering() {
    let mut f = listed(&[]);
    f.view.begin_lifecycle(Ask::Archive);
    let (request, _) = f.wrote();
    f.reply(request, Err(Failure::lost()));
    assert_eq!(
        f.view.lifecycle_said().map(|said| said.words),
        Some(Words::BotsLifecycleUnanswered)
    );
    assert!(f.view.lifecycle_busy(), "still on its way");
}

/// The read of the Bot the tab just sent, past the other reads a lifecycle
/// answer leaves owed (its Trash filter, its list).
fn bot_read(f: &Fixture) -> u64 {
    loop {
        match f.commands.try_recv().expect("the Bot is read") {
            Command::Read { seq, .. } => return seq,
            _ => continue,
        }
    }
}

/// A Bot whose conversation's Trash is parked, as the owner's `read_bot`
/// answers it (`Bot.open_operation`, the operation waiting for the person).
fn parked_conversation_trash() -> (Bot, BotRequestId) {
    let mut held = unpaused();
    let parked = BotRequestId::mint();
    held.open_operation = Some(koru_ai_core::bots::BotOpenLifecycle {
        request_id: parked,
        kind: BotLifecycleKind::TrashChat,
        chat_id: Some(ChatId::new()),
        stage: BotLifecycleStage::Incomplete,
        refusal: Some(BotLifecycleRefusal::WorkRunning),
    });
    (held, parked)
}

/// Whether the tab offers a parked operation's two answers.
fn offers_continue_and_cancel(f: &Fixture) -> bool {
    let keys = f.keys();
    keys.contains(&super::super::actions::CONTINUE)
        && keys.contains(&super::super::actions::CANCEL_OPERATION)
}

/// **A tab's first read of its Bot adopts the open operation** — the second
/// review's fault 2. A tab restored at startup, one opened from a link and
/// one opened from the roster each read the Bot once when first shown, and
/// that first read shows the parked conversation Trash with Continue and
/// Cancel and offers no Archive or Trash, which the owner would refuse.
#[test]
fn a_tabs_first_read_adopts_the_open_operation_however_it_was_opened() {
    let (held, _) = parked_conversation_trash();
    // Restored at startup (`BotView::restored`, here with the test's link).
    let f = Fixture::opened(held.clone());
    assert!(
        f.view.lifecycle_busy(),
        "restored: adopted from the first read"
    );
    assert_eq!(
        f.view.lifecycle_said().map(|said| said.words),
        Some(Words::BotsChatTrashIncomplete)
    );
    assert!(offers_continue_and_cancel(&f), "restored");
    assert!(f.view.lifecycle_rows().is_empty(), "no Archive or Trash");
    // Opened from a link, before the roster listed it.
    for opened in ["link", "roster"] {
        let mut f = fixture();
        f.bot = held.clone();
        let (link, commands, answers) = BotsLink::test_pair();
        f.view = match opened {
            "link" => BotView::labelled(held.id, held.main_chat, "Scout", None),
            _ => BotView::opened(&RosterBot::from(held.clone())),
        };
        f.view.link = Some(link);
        f.view.started_link = true;
        f.commands = commands;
        f.answers = answers;
        f.view.set_viewport(800.0, 1600.0);
        // Its first showing reads the Bot once — the roster's row carries no
        // open operation.
        f.view.shown_pass(None);
        let seq = bot_read(&f);
        f.answer(seq, held.id, Ok(held.clone()));
        assert!(f.view.lifecycle_busy(), "{opened}: adopted");
        assert!(offers_continue_and_cancel(&f), "{opened}");
        assert!(f.view.lifecycle_rows().is_empty(), "{opened}");
    }
}

/// **A settings save keeps a conversation's parked Trash** — the second
/// review's fault 3, turned round. The owner answers an `Update` with the
/// Bot it wrote (`execute_bot_command`), which leaves `open_operation` out,
/// and a conversation's Trash does not block a save (`bot_hold`): that
/// answer is a write's, so the parked Trash and its Continue and Cancel
/// stay until a read says otherwise.
#[test]
fn a_settings_save_keeps_a_conversations_parked_trash() {
    use winit::keyboard::NamedKey;
    let (held, parked) = parked_conversation_trash();
    let mut f = Fixture::opened(held.clone());
    assert!(f.view.lifecycle_busy());
    while f.view.focus() != Some(NAME_FIELD) {
        assert!(key(&mut f.view, named(NamedKey::Tab), None));
    }
    let input = f.view.name_input();
    crate::input_engine::with(|engine| input.borrow_mut().select_all(engine));
    assert!(key(
        &mut f.view,
        WinitKey::Character("Scout two".into()),
        Some("Scout two")
    ));
    assert!(key(&mut f.view, named(NamedKey::Enter), None));
    let (request, sent) = f.wrote();
    assert!(matches!(sent, BotCommand::Update { .. }));
    let mut saved = held.clone();
    saved.config.name = "Scout two".into();
    saved.revision += 1;
    saved.open_operation = None;
    f.reply(request, Ok(BotResponse::Bot { bot: saved.clone() }));
    assert_eq!(f.view.label(), "Scout two", "the save is shown");
    assert!(
        f.view.lifecycle_busy(),
        "the parked Trash is still followed"
    );
    assert_eq!(
        f.view.lifecycle_said().map(|said| said.words),
        Some(Words::BotsChatTrashIncomplete)
    );
    assert!(offers_continue_and_cancel(&f));
    // Continue sends the parked operation again, by its own request.
    assert!(f.view.continue_lifecycle());
    let (again, _) = f.wrote();
    assert_eq!(again, parked);
}

/// **A Restore's read that fails is asked again on the bounded clock**
/// while the tab is shown (the second review's item 9): until it answers,
/// nothing is offered, and a lost answer must not leave the tab offering
/// nothing for ever.
#[test]
fn a_failed_read_after_a_restore_is_asked_again_on_the_clock() {
    let mut f = listed(&[]);
    f.view.begin_lifecycle(Ask::Trash);
    let (trash, _) = f.wrote();
    let bot = f.bot.clone();
    f.reply(
        trash,
        progress(
            &bot,
            trash,
            BotLifecycleKind::Trash,
            BotLifecycleStage::Completed,
        ),
    );
    let mut trashed = f.bot.clone();
    trashed.lifecycle = BotLifecycle::Trashed;
    f.view.take_trash_listing(Ok(vec![trashed.clone()]));
    assert!(f.view.perform(region::Act::Restore));
    let (undo, _) = f.wrote();
    f.reply(
        undo,
        progress(
            &trashed,
            undo,
            BotLifecycleKind::Restore,
            BotLifecycleStage::Completed,
        ),
    );
    f.view.shown_pass(None);
    let seq = bot_read(&f);
    f.answer(seq, f.bot.id, Err(Failure::lost()));
    assert!(f.view.lifecycle_rows().is_empty(), "still nothing claimed");
    assert!(
        f.view.maintenance_deadline().is_some(),
        "a wake is owed for the read"
    );
    let later = std::time::Instant::now() + std::time::Duration::from_secs(3);
    assert!(f.view.bot_read_due(later), "the read is due again");
    f.view.shown_pass(None);
    let seq = bot_read(&f);
    let active = f.bot.clone();
    f.answer(seq, active.id, Ok(active));
    assert_eq!(f.view.lifecycle_rows(), [Ask::Archive, Ask::Trash]);
}

/// **The unsent draft survives a Trash and its Restore** (the second
/// review's item 7): a trashed Bot's conversation reads nothing — its
/// client stopped, the window's passes not reaching it — but the view, and
/// the words the person had typed and not sent, are kept and shown again
/// when the Restore's read binds it.
#[test]
fn an_unsent_draft_survives_a_trash_and_its_restore() {
    let mut f = listed(&[]);
    for letter in ["h", "i"] {
        assert!(key(
            &mut f.view,
            WinitKey::Character(letter.into()),
            Some(letter)
        ));
    }
    assert_eq!(
        f.view.conversation_mut().expect("bound").composer_text(),
        "hi"
    );
    // Trashed elsewhere: the Trash filter names it, and its Trash
    // (`with_lifecycle_holders`).
    let mut trashed = f.bot.clone();
    trashed.lifecycle = BotLifecycle::Trashed;
    trashed.held_by = Some(koru_ai_core::bots::BotLifecycleHolder {
        request_id: BotRequestId::mint(),
        kind: BotLifecycleKind::Trash,
        completed_at_ms: 1,
    });
    f.view.take_trash_listing(Ok(vec![trashed.clone()]));
    assert!(
        f.view.conversation().is_none(),
        "let go: nothing reaches it"
    );
    assert!(f.view.perform(region::Act::Restore));
    let (undo, _) = f.wrote();
    f.reply(
        undo,
        progress(
            &trashed,
            undo,
            BotLifecycleKind::Restore,
            BotLifecycleStage::Completed,
        ),
    );
    f.view.shown_pass(None);
    let seq = bot_read(&f);
    let active = f.bot.clone();
    f.answer(seq, active.id, Ok(active));
    assert_eq!(
        f.view
            .conversation_mut()
            .expect("bound again")
            .composer_text(),
        "hi",
        "the draft is the person's still"
    );
}

/// **A tab left in front of a parked Archive sees the owner finish it** —
/// the third review's blocker, through the tab. A tab restored after a
/// restart adopts the Archive parked for its silent owner; the owner takes
/// it up again by itself (`owner_answered_bot_lifecycle`) and completes it.
/// The tab reads it while shown, and once the owner says completed it says
/// the Bot is archived and offers Unarchive — no Continue left behind.
#[test]
fn a_tab_in_front_of_a_parked_archive_sees_the_owner_finish_it() {
    let mut held = unpaused();
    let parked = BotRequestId::mint();
    held.open_operation = Some(koru_ai_core::bots::BotOpenLifecycle {
        request_id: parked,
        kind: BotLifecycleKind::Archive,
        chat_id: None,
        stage: BotLifecycleStage::Incomplete,
        refusal: Some(BotLifecycleRefusal::OwnerUnavailable),
    });
    let mut f = Fixture::opened(held.clone());
    f.view.set_settings_open(false);
    assert!(offers_continue_and_cancel(&f));
    // Shown, its clock due: a read of the parked Archive.
    f.view.lifecycle_read_now();
    f.view.shown_pass(None);
    let (asked, read) = f.wrote();
    assert_eq!(asked, parked);
    assert!(matches!(read, BotCommand::ReadLifecycleOperation { .. }));
    f.reply(
        parked,
        progress(
            &held,
            parked,
            BotLifecycleKind::Archive,
            BotLifecycleStage::Completed,
        ),
    );
    assert!(!f.view.lifecycle_busy(), "finished");
    let keys = f.keys();
    assert!(
        !keys.contains(&actions::CONTINUE),
        "no Continue left behind"
    );
    assert!(keys.contains(&actions::UNARCHIVE), "Unarchive names it");
    assert_eq!(f.view.lifecycle_rows(), [Ask::Unarchive, Ask::Trash]);
}
