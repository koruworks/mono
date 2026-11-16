// SPDX-License-Identifier: MPL-2.0

//! The window's half of a Bot's quick setup: the Role page is read before
//! the card is offered and never replaced once a person wrote it, a Save's
//! write goes to a worker once and its answer reaches the Bot's tab, and a
//! setup that was ended stays ended through the session file.

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use koru_ai_core::bots::Bot;
use koru_store_core::ids::WorkspaceId;
use koru_workspace_host::{Settings, View};

use super::*;
use crate::bots::BotView;
use crate::bots::onboard::{RoleKnown, RoleSave, Stage};
use crate::bots::roster::RosterBot;
use crate::bots::view::onboard as ui;
use crate::mainview::harness::*;

/// What a window handed its workers: each read and each write, with the end
/// a test answers.
#[derive(Default)]
struct Asked {
    reads: Vec<(RoleRead, Sender<Result<bool, String>>)>,
    writes: Vec<(RoleWrite, Sender<Result<(), String>>)>,
}

type Shared = Arc<Mutex<Asked>>;

struct FakePort(Shared);

impl RolePort for FakePort {
    fn read(&mut self, _reach: Reach<'_>, read: RoleRead) -> Receiver<Result<bool, String>> {
        let (send, receive) = mpsc::channel();
        self.0
            .lock()
            .expect("the test's list")
            .reads
            .push((read, send));
        receive
    }

    fn write(&mut self, _reach: Reach<'_>, write: RoleWrite) -> Receiver<Result<(), String>> {
        let (send, receive) = mpsc::channel();
        self.0
            .lock()
            .expect("the test's list")
            .writes
            .push((write, send));
        receive
    }
}

fn a_bot() -> Bot {
    crate::bots::tests::bot(BotId::mint(), WorkspaceId::new(), "Scout", false)
}

/// A window with `bot`'s tab in front, its Bot read, its main conversation
/// read and empty, and workers the test answers.
fn window_on(bot: &Bot) -> (MainView, koru_workspace::Workspace, Shared) {
    let (mut v, w) = container();
    let asked = Shared::default();
    *v.bot_setups_mut() = BotSetups::with_port(Box::new(FakePort(asked.clone())));
    assert!(v.open_bot_tab(RosterBot::from(bot.clone())));
    let view = tab(&mut v);
    view.know_bot_for_test(bot.clone());
    View::set_viewport(view, 1000.0, 1400.0);
    view.shown_pass(None);
    view.conversation_mut()
        .expect("the conversation is bound")
        .pretend_read(bot.main_chat);
    (v, w, asked)
}

fn tab(v: &mut MainView) -> &mut BotView {
    let TabView::Bot(view) = v.active_view() else {
        panic!("the Bot tab is in front");
    };
    view
}

/// The window says the setup is on offer, the tab asks about its Role page,
/// and the worker answers `role`: blank, written, or a failure.
fn role_read(v: &mut MainView, asked: &Shared, role: Result<bool, String>) {
    v.maintain_bot_setup();
    tab(v).settle_setup();
    assert!(v.maintain_bot_setup(), "the read goes to a worker");
    let (read, answer) = asked.lock().unwrap().reads.remove(0);
    assert_eq!(read.bot, tab(v).bot().expect("a Bot"));
    answer.send(role).unwrap();
    v.maintain_bot_setup();
    tab(v).settle_setup();
}

fn a_page(markdown: &str) -> WorkspaceDoc {
    let bytes =
        koru_import_markdown::document_snapshot(koru_core::WorkspaceId::new(), "Scout", markdown)
            .expect("a page");
    WorkspaceDoc::from_snapshot(&bytes).expect("it opens")
}

/// **A page is blank only with nothing under its title.** The page a new
/// Bot is made with is; a page with a sentence, a list or a heading is not.
#[test]
fn a_role_page_is_blank_only_with_nothing_under_its_title() {
    assert!(role_is_blank(&a_page("")), "a new Bot's Role page");
    assert!(role_is_blank(&a_page("\n\n")));
    for written in [
        "Review my specs.",
        "# Role\n",
        "- one\n- two\n",
        "\n\nlater",
        "```\ncode\n```",
    ] {
        assert!(!role_is_blank(&a_page(written)), "{written:?}");
    }
}

/// **A Role a person already wrote is never replaced.** The card is not
/// offered for it: no field, no Save, nothing a press or a client could
/// reach, and no write ever goes to a worker.
#[test]
fn a_bot_with_a_written_role_gets_no_card_and_no_write() {
    let bot = a_bot();
    let (mut v, _w, asked) = window_on(&bot);
    role_read(&mut v, &asked, Ok(false));
    assert_eq!(tab(&mut v).setup().role_known(), RoleKnown::Written);
    assert!(!tab(&mut v).setup_shown());
    assert!(
        !tab(&mut v).accessibility_set_value(ui::ROLE_FIELD, "A new role"),
        "there is no field"
    );
    assert!(!tab(&mut v).accessibility_action(ui::SAVE_ROLE, true));
    v.maintain_bot_setup();
    let asked = asked.lock().unwrap();
    assert!(asked.writes.is_empty(), "nothing was written");
    assert!(
        asked.reads.is_empty(),
        "and the page is not asked about again"
    );
}

/// **A Role read that is pending or failed is never taken for empty.** No
/// card, so no Save; a failed read is asked again when the tab is next
/// shown, and the card comes once the page reads as empty.
#[test]
fn a_pending_or_failed_role_read_offers_no_save() {
    let bot = a_bot();
    let (mut v, _w, asked) = window_on(&bot);
    v.maintain_bot_setup();
    tab(&mut v).settle_setup();
    assert!(v.maintain_bot_setup());
    assert_eq!(tab(&mut v).setup().role_known(), RoleKnown::Reading);
    tab(&mut v).settle_setup();
    assert!(!tab(&mut v).setup_shown(), "pending: no card");
    assert!(!tab(&mut v).accessibility_action(ui::SAVE_ROLE, true));
    assert!(tab(&mut v).take_role_write().is_none());

    let (_, answer) = asked.lock().unwrap().reads.remove(0);
    answer.send(Err("store_unavailable".into())).unwrap();
    assert!(v.maintain_bot_setup());
    tab(&mut v).settle_setup();
    assert_eq!(tab(&mut v).setup().role_known(), RoleKnown::Failed);
    assert!(!tab(&mut v).setup_shown(), "failed: no card");
    assert!(!tab(&mut v).accessibility_action(ui::SAVE_ROLE, true));
    v.maintain_bot_setup();
    assert!(
        asked.lock().unwrap().reads.is_empty(),
        "not asked in a loop"
    );
    assert!(asked.lock().unwrap().writes.is_empty());

    // Hidden and shown again: asked once more, and now it answers.
    tab(&mut v).hidden();
    role_read(&mut v, &asked, Ok(true));
    assert_eq!(tab(&mut v).setup().role_known(), RoleKnown::Empty);
    assert!(tab(&mut v).setup_shown());
}

/// **A Save is written by a worker, once, and its answer reaches the tab.**
/// The window's pass hands the write over and returns; nothing is written
/// on the window thread, a second pass sends nothing more, a failure is
/// shown for a retry, and the retry's answer moves the card on. The card's
/// first write is marked to go only into a page still empty.
#[test]
fn a_role_save_goes_to_a_worker_once_and_its_answer_reaches_the_tab() {
    let bot = a_bot();
    let (mut v, _w, asked) = window_on(&bot);
    role_read(&mut v, &asked, Ok(true));
    assert!(
        tab(&mut v).setup_shown(),
        "an empty Role: the card is offered"
    );

    assert!(tab(&mut v).accessibility_action(ui::SAVE_ROLE, true) == false);
    assert!(tab(&mut v).accessibility_set_value(ui::ROLE_FIELD, "Review my specs."));
    tab(&mut v).settle_setup();
    assert!(tab(&mut v).accessibility_action(ui::SAVE_ROLE, true));
    assert!(
        asked.lock().unwrap().writes.is_empty(),
        "nothing before the pass"
    );
    assert!(v.maintain_bot_setup(), "the pass hands the write over");
    {
        let asked = asked.lock().unwrap();
        assert_eq!(asked.writes.len(), 1, "one write");
        let (write, _) = &asked.writes[0];
        assert_eq!(write.bot, bot.id);
        assert_eq!(write.page, bot.config.role, "the Bot's Role page");
        assert_eq!(write.markdown, "Review my specs.");
        assert!(write.only_if_blank, "the first write looks again first");
    }
    assert!(!v.maintain_bot_setup(), "no answer yet: nothing moves");
    assert_eq!(asked.lock().unwrap().writes.len(), 1, "nothing sent twice");
    assert_eq!(tab(&mut v).setup().role_save(), RoleSave::Saving);

    // The Store refused: the card says so and offers Retry.
    let (_, answer) = asked.lock().unwrap().writes.remove(0);
    answer.send(Err("store_unavailable".into())).unwrap();
    assert!(v.maintain_bot_setup());
    assert_eq!(tab(&mut v).setup().role_save(), RoleSave::Failed);
    assert_eq!(tab(&mut v).setup().stage(), Stage::Role);

    tab(&mut v).settle_setup();
    assert!(tab(&mut v).accessibility_action(ui::SAVE_ROLE, true));
    assert!(v.maintain_bot_setup());
    let (retry, answer) = asked.lock().unwrap().writes.remove(0);
    assert_eq!(retry.markdown, "Review my specs.");
    assert!(
        retry.only_if_blank,
        "still nothing of the card's on the page"
    );
    answer.send(Ok(())).unwrap();
    assert!(v.maintain_bot_setup());
    assert_eq!(tab(&mut v).setup().role_save(), RoleSave::Saved);
    assert_eq!(tab(&mut v).setup().stage(), Stage::Context);

    // Back on step 1, untouched: Save writes nothing.
    tab(&mut v).settle_setup();
    assert!(tab(&mut v).accessibility_action(ui::STEP_ROLE, true));
    tab(&mut v).settle_setup();
    assert!(tab(&mut v).accessibility_action(ui::SAVE_ROLE, true));
    v.maintain_bot_setup();
    assert!(asked.lock().unwrap().writes.is_empty(), "unchanged text");

    // Changed: the card's own text is replaced, with no second look.
    tab(&mut v).settle_setup();
    assert!(tab(&mut v).accessibility_action(ui::STEP_ROLE, true));
    tab(&mut v).settle_setup();
    assert!(tab(&mut v).accessibility_set_value(ui::ROLE_FIELD, "Review my specs, kindly."));
    tab(&mut v).settle_setup();
    assert!(tab(&mut v).accessibility_action(ui::SAVE_ROLE, true));
    v.maintain_bot_setup();
    let (edit, _) = asked.lock().unwrap().writes.remove(0);
    assert!(!edit.only_if_blank, "an edit of what the card wrote");
}

/// **A Role written while the card was open is not replaced either.** The
/// worker looks again before the card's first write; when it finds the page
/// written it writes nothing, and the card goes.
#[test]
fn a_role_written_meanwhile_ends_the_card_without_a_write() {
    let bot = a_bot();
    let (mut v, _w, asked) = window_on(&bot);
    role_read(&mut v, &asked, Ok(true));
    tab(&mut v).accessibility_set_value(ui::ROLE_FIELD, "Keep a digest.");
    tab(&mut v).settle_setup();
    assert!(tab(&mut v).accessibility_action(ui::SAVE_ROLE, true));
    assert!(v.maintain_bot_setup());
    let (_, answer) = asked.lock().unwrap().writes.remove(0);
    answer.send(Err(NOT_EMPTY.into())).unwrap();
    assert!(v.maintain_bot_setup());
    tab(&mut v).settle_setup();
    assert_eq!(tab(&mut v).setup().role_known(), RoleKnown::Written);
    assert_eq!(tab(&mut v).setup().role_save(), RoleSave::Idle);
    assert!(!tab(&mut v).setup_shown());
}

/// **A worker that never answers is a failed write**, not a card pending
/// for ever: its channel closes and the pass reads that.
#[test]
fn a_worker_that_dies_is_a_failed_write() {
    let bot = a_bot();
    let (mut v, _w, asked) = window_on(&bot);
    role_read(&mut v, &asked, Ok(true));
    tab(&mut v).accessibility_set_value(ui::ROLE_FIELD, "Keep a digest.");
    tab(&mut v).settle_setup();
    assert!(tab(&mut v).accessibility_action(ui::SAVE_ROLE, true));
    assert!(v.maintain_bot_setup());
    asked.lock().unwrap().writes.clear();
    assert!(v.maintain_bot_setup());
    assert_eq!(tab(&mut v).setup().role_save(), RoleSave::Failed);
}

/// **A setup that was ended stays ended through a restart.** The tab's end
/// reaches the process's book on the window's pass; the book is the
/// session file's `bot_setup=` line; and a window that restores that line
/// tells the Bot's tab, made again, that its setup is over.
#[test]
fn an_ended_setup_survives_the_session_file() {
    let bot = a_bot();
    let (mut v, _w, asked) = window_on(&bot);
    role_read(&mut v, &asked, Ok(true));
    assert!(tab(&mut v).setup_shown());
    assert!(!onboard::dismissed(bot.id));

    assert!(tab(&mut v).accessibility_action(ui::SKIP_ALL, true));
    assert!(v.maintain_bot_setup(), "the pass writes the book");
    assert!(onboard::dismissed(bot.id));

    let mut settings = Settings::default();
    View::capture(&v, &mut settings);
    let id = bot.id.as_uuid().simple().to_string();
    assert!(settings.bot_setup.contains(&id), "{}", settings.bot_setup);
    // (That the line survives the file itself is the settings' own test:
    // `the_bot_setup_line_round_trips_and_is_absent_when_empty`.)

    // The process ends: its book is gone. A new one reads the file's line.
    onboard::note(bot.id, false);
    assert!(!onboard::dismissed(bot.id));
    // What `MainView::restore` does with the settings it is handed.
    restore(&settings.bot_setup);
    assert!(onboard::dismissed(bot.id), "restored from the line");

    let (mut again, _w, again_asked) = window_on(&bot);
    again.maintain_bot_setup();
    tab(&mut again).settle_setup();
    again.maintain_bot_setup();
    assert!(!tab(&mut again).setup_shown(), "ended stays ended");
    assert!(
        again_asked.lock().unwrap().reads.is_empty(),
        "an ended setup reads nothing"
    );

    // The way back in is remembered the same way.
    tab(&mut v).settle_setup();
    assert!(tab(&mut v).accessibility_action(ui::AGAIN, true));
    assert!(v.maintain_bot_setup());
    assert!(!onboard::dismissed(bot.id), "opened again");
}
