// SPDX-License-Identifier: MPL-2.0

use koru_i18n::{Lang, t};

use super::*;

fn page() -> BotPage {
    BotPage {
        workspace: koru_store_core::ids::WorkspaceId::new(),
        document: koru_ai_core::DocumentId::new(),
    }
}

fn save(setup: &mut Setup, bot: BotId) -> bool {
    setup.save(bot, page(), "Scout".into(), "before".into(), "after".into())
}

/// A card on offer: not ended on this install, its Role page read as empty.
fn offered() -> Setup {
    let mut setup = Setup::default();
    setup.tell(false);
    assert!(setup.read_role(BotId::mint(), page()));
    let read = setup.take_read().expect("a read");
    assert!(setup.role_read(read.serial, Some(true)));
    setup
}

/// **Nothing shows until the window has said** whether the card was ended
/// **and the Role page has read as empty**: a tab made before either is
/// known never flashes a card, and an ended setup reads nothing.
#[test]
fn nothing_shows_until_the_window_has_said_and_the_role_is_read() {
    let bot = BotId::mint();
    let mut setup = Setup::default();
    assert!(!setup.active());
    assert!(!setup.read_role(bot, page()), "not told: nothing is read");
    assert!(setup.tell(true));
    assert!(!setup.active(), "ended on this install");
    assert!(!setup.read_role(bot, page()), "ended: nothing is read");
    assert!(setup.tell(false));
    assert!(!setup.tell(false), "said twice, changes once");
    assert!(!setup.active(), "the Role page is not read yet");
    assert!(setup.read_role(bot, page()));
    assert!(!setup.read_role(bot, page()), "asked once");
    assert!(!setup.active(), "still reading");
    let read = setup.take_read().expect("one read");
    assert!(setup.take_read().is_none());
    assert!(
        !setup.role_read(read.serial + 1, Some(true)),
        "not this read"
    );
    assert!(setup.role_read(read.serial, Some(true)));
    assert!(setup.active());
    assert!(!setup.role_read(read.serial, Some(false)), "answered once");
}

/// **A Role a person already wrote is never the card's**: read as written,
/// the card does not show and Save does nothing, whatever is typed. A read
/// that is pending or failed is not taken for empty either; a failed one is
/// asked again when the tab is shown again.
#[test]
fn a_written_pending_or_failed_role_offers_no_card_and_no_save() {
    let bot = BotId::mint();
    for answer in [Some(false), None] {
        let mut setup = Setup::default();
        setup.tell(false);
        setup.edit("A role typed before anything is known.");
        assert!(!setup.can_save(), "not read yet");
        assert!(!save(&mut setup, bot));
        assert!(setup.read_role(bot, page()));
        assert!(!setup.can_save(), "pending");
        assert!(!save(&mut setup, bot));
        let read = setup.take_read().expect("a read");
        assert!(setup.role_read(read.serial, answer));
        assert!(!setup.active(), "{answer:?}: no card");
        assert!(!setup.can_save(), "{answer:?}: no save");
        assert!(!save(&mut setup, bot));
        assert!(setup.take_write().is_none(), "{answer:?}: nothing written");
        match answer {
            Some(_) => {
                assert_eq!(setup.role_known(), RoleKnown::Written);
                assert!(!setup.retry_role_read(), "known: not asked again");
                assert!(!setup.read_role(bot, page()));
            }
            None => {
                assert_eq!(setup.role_known(), RoleKnown::Failed);
                assert!(!setup.read_role(bot, page()), "not in a loop");
                assert!(setup.retry_role_read());
                assert!(setup.read_role(bot, page()), "asked again when shown");
            }
        }
    }
}

/// **The card's first write goes only into a page still empty**; once its
/// own text is there, an edit replaces it. A worker that finds the page
/// written meanwhile writes nothing, and the card is over.
#[test]
fn the_first_write_is_guarded_and_a_role_written_meanwhile_ends_the_card() {
    let bot = BotId::mint();
    let mut setup = offered();
    setup.edit("Review my specs.");
    assert!(save(&mut setup, bot));
    let first = setup.take_write().expect("a write");
    assert!(first.only_if_blank);
    assert!(setup.written(first.serial, true));
    assert!(setup.go(Stage::Role));
    setup.edit("Review my specs, kindly.");
    assert!(save(&mut setup, bot));
    assert!(!setup.take_write().expect("an edit").only_if_blank);

    let mut raced = offered();
    raced.edit("Keep a digest.");
    assert!(save(&mut raced, bot));
    let write = raced.take_write().expect("a write");
    assert!(!raced.found_written(write.serial + 1));
    assert!(raced.found_written(write.serial));
    assert_eq!(raced.role_known(), RoleKnown::Written);
    assert_eq!(raced.role_save(), RoleSave::Idle);
    assert!(!raced.active());
    assert!(!raced.can_save());
}

/// **Save writes the Role once**: one write per press, none while one is on
/// its way, pending until the worker answers, and on to step 2 when it
/// does. Text the Store already took is never written again.
#[test]
fn save_writes_the_role_once_and_shows_pending_then_saved() {
    let bot = BotId::mint();
    let mut setup = offered();
    assert!(!setup.can_save(), "nothing typed");
    assert!(!save(&mut setup, bot));
    assert!(setup.edit("  \n "));
    assert!(!setup.can_save(), "spaces are not a role");

    assert!(setup.edit("Review my specs."));
    assert!(save(&mut setup, bot));
    assert_eq!(setup.role_save(), RoleSave::Saving);
    let write = setup.take_write().expect("one write");
    assert_eq!(write.markdown, "Review my specs.");
    assert_eq!(write.bot, bot);
    assert!(setup.take_write().is_none(), "taken once");
    assert!(!save(&mut setup, bot), "a second press sends nothing");
    assert!(setup.take_write().is_none());
    assert_eq!(setup.stage(), Stage::Role, "still pending");

    assert!(!setup.written(write.serial + 1, true), "not this write");
    assert!(setup.written(write.serial, true));
    assert_eq!(setup.role_save(), RoleSave::Saved);
    assert_eq!(setup.stage(), Stage::Context);
    assert_eq!(setup.mark(Stage::Role, false), Mark::Done);
    assert!(!setup.written(write.serial, true), "answered once");

    // Back on step 1 with the same words: Save moves on and writes nothing.
    assert!(setup.go(Stage::Role));
    assert!(save(&mut setup, bot));
    assert!(setup.take_write().is_none(), "already what the Store holds");
    assert_eq!(setup.stage(), Stage::Context);

    // Other words are a new write.
    assert!(setup.go(Stage::Role));
    assert!(setup.edit("Review my specs, kindly."));
    assert_eq!(setup.role_save(), RoleSave::Idle);
    assert!(save(&mut setup, bot));
    assert_eq!(setup.take_write().map(|write| write.serial), Some(2));
}

/// **A failed write is said and can be retried**: the card stays on step
/// 1, and the next press is a new write of the same words.
#[test]
fn a_failed_save_is_shown_and_can_be_retried() {
    let bot = BotId::mint();
    let mut setup = offered();
    setup.edit("Keep a digest.");
    assert!(save(&mut setup, bot));
    let first = setup.take_write().expect("a write");
    assert!(setup.written(first.serial, false));
    assert_eq!(setup.role_save(), RoleSave::Failed);
    assert_eq!(setup.stage(), Stage::Role);
    assert_eq!(setup.mark(Stage::Role, false), Mark::Current);
    assert!(setup.can_save(), "Retry is offered");

    assert!(save(&mut setup, bot));
    let second = setup.take_write().expect("the retry");
    assert_ne!(second.serial, first.serial);
    assert_eq!(second.markdown, first.markdown);
    assert!(
        !setup.written(first.serial, true),
        "the old answer is stale"
    );
    assert!(setup.written(second.serial, true));
    assert_eq!(setup.stage(), Stage::Context);
}

/// **A tick means saved, not passed**: a skipped step keeps its number, the
/// step shown is the current one, and Knowledge the Bot has makes step 2
/// done.
#[test]
fn the_stepper_says_done_current_and_waiting() {
    let mut setup = offered();
    assert_eq!(setup.mark(Stage::Role, false), Mark::Current);
    assert_eq!(setup.mark(Stage::Context, false), Mark::Waiting);
    assert!(setup.skip_role());
    assert_eq!(setup.mark(Stage::Role, false), Mark::Waiting, "skipped");
    assert_eq!(setup.mark(Stage::Context, false), Mark::Current);
    assert!(setup.go(Stage::Role), "every step is reachable");
    assert_eq!(setup.mark(Stage::Context, true), Mark::Done);
    assert!(!setup.go(Stage::Role), "already there");
    assert!(setup.finish());
    assert_eq!(setup.mark(Stage::Role, false), Mark::Waiting);
    assert_eq!(setup.mark(Stage::Context, false), Mark::Waiting);
    assert_eq!(setup.mark(Stage::Context, true), Mark::Done);
}

/// **Done, a skip and a first message each end the card**, each tells the
/// book once, and the finished card stays for this showing with its way
/// back in.
#[test]
fn finish_skip_and_a_first_message_end_the_card() {
    // Finish (and *Skip setup*, the same act): the card says ready.
    let mut setup = offered();
    assert!(setup.finish());
    assert_eq!(setup.take_news(), Some(News::Dismissed));
    assert_eq!(setup.take_news(), None, "told once");
    assert_eq!(setup.stage(), Stage::Done);
    assert!(setup.active(), "the finished card stays for this showing");
    assert!(!setup.finish(), "ended once");
    // The book now says ended: nothing moves, the card still stays.
    assert!(!setup.tell(true));
    assert!(setup.active());

    // The way back in opens it again, here and in the book.
    assert!(setup.go(Stage::Role));
    assert_eq!(setup.take_news(), Some(News::Reopened));
    assert_eq!(setup.stage(), Stage::Role);
    assert!(setup.active());

    // A first message: gone at once, with nothing left to say.
    assert!(setup.sent());
    assert_eq!(setup.take_news(), Some(News::Dismissed));
    assert!(!setup.active());
    assert!(!setup.sent(), "already over");

    // A message after Done ends the finished card too, and tells nothing
    // new.
    let mut done = offered();
    done.finish();
    done.take_news();
    assert!(done.sent());
    assert_eq!(done.take_news(), None);
    assert!(!done.active());
}

/// **The book survives the session file**: what was ended is ended after a
/// write and a read, a Bot opened again is not, a damaged entry costs only
/// itself, and the book stays bounded.
#[test]
fn the_book_round_trips_through_its_line() {
    let (a, b, c) = (BotId::mint(), BotId::mint(), BotId::mint());
    let mut book = SetupBook::default();
    assert_eq!(book.to_storage(), "", "empty writes no line");
    assert!(book.note(a, true));
    assert!(!book.note(a, true), "noted once");
    assert!(book.note(b, true));
    assert!(book.note(b, false), "opened again");
    assert!(!book.note(c, false));

    let line = book.to_storage();
    assert!(!line.contains(['=', '\n', ' ']), "one value of one line");
    let back = SetupBook::parse(&line);
    assert_eq!(back, book);
    assert!(back.contains(a));
    assert!(!back.contains(b));

    let damaged = format!("nonsense,,{line},also-not-an-id");
    assert_eq!(SetupBook::parse(&damaged), book);

    let mut merged = SetupBook::default();
    merged.note(c, true);
    assert!(merged.merge(back));
    assert!(merged.contains(a) && merged.contains(c));

    let mut full = SetupBook::default();
    let first = BotId::mint();
    full.note(first, true);
    for _ in 0..BOOK_LIMIT {
        full.note(BotId::mint(), true);
    }
    assert!(!full.contains(first), "the oldest entry went");
    assert_eq!(SetupBook::parse(&full.to_storage()), full);
}

/// **The process's book is what the session file is written from**, and a
/// restore merges into it.
#[test]
fn the_process_book_is_restored_and_captured() {
    let (here, restored) = (BotId::mint(), BotId::mint());
    assert!(!dismissed(here));
    assert!(note(here, true));
    let mut other = SetupBook::default();
    other.note(restored, true);
    restore(&other.to_storage());
    restore("");
    assert!(dismissed(here), "a restore never forgets");
    assert!(dismissed(restored));
    let line = capture();
    let read = SetupBook::parse(&line);
    assert!(read.contains(here) && read.contains(restored));
    assert!(note(here, false));
    assert!(!dismissed(here));
    note(restored, false);
}

/// **Every word the card says exists in all eight languages**, each
/// carries the placeholders its English does, and the examples promise no
/// schedule and no trigger: a new Bot has neither without a Routine.
#[test]
fn every_new_word_is_in_all_eight_languages() {
    let words = [
        Words::BotsSetupGreeting,
        Words::BotsSetupTitle,
        Words::BotsSetupSkipAll,
        Words::BotsSetupSteps,
        Words::BotsSetupStepRole,
        Words::BotsSetupStepContext,
        Words::BotsSetupStepIsDone,
        Words::BotsSetupStepIsCurrent,
        Words::BotsSetupStepIsWaiting,
        Words::BotsSetupRoleLabel,
        Words::BotsSetupRolePlaceholder,
        Words::BotsSetupRoleNote,
        Words::BotsSetupTry,
        Words::BotsSetupExampleDigest,
        Words::BotsSetupExampleTasks,
        Words::BotsSetupExampleSpecs,
        Words::BotsSetupExampleDigestText,
        Words::BotsSetupExampleTasksText,
        Words::BotsSetupExampleSpecsText,
        Words::BotsSetupRoleFailed,
        Words::BotsSetupSkip,
        Words::BotsSetupContextHint,
        Words::BotsSetupBrowse,
        Words::BotsSetupDone,
        Words::BotsSetupReady,
        Words::BotsSetupAgain,
        Words::BotsSetupHint,
        Words::BotsSetupHistoryBefore,
        Words::BotsSetupHistoryAfter,
    ];
    assert_eq!(words.len(), 29);
    for key in words {
        let english = t(Lang::En, key);
        for lang in Lang::ALL {
            let said = t(lang, key);
            assert!(!said.trim().is_empty(), "{key:?} in {}", lang.tag());
            assert_eq!(
                said.matches("{}").count(),
                english.matches("{}").count(),
                "{key:?} in {}",
                lang.tag()
            );
            if lang != Lang::En && english.split_whitespace().count() > 3 {
                assert_ne!(said, english, "{key:?} is English in {}", lang.tag());
            }
        }
    }
    for example in Example::ALL {
        assert!(!t(Lang::En, example.label()).is_empty());
    }
}

/// **The words say only what a Bot does.** The mock-up's sentences that
/// were not true are not in the table: nothing claims the Bot sees nothing
/// else, works its own setup out, or runs on a schedule or a trigger.
#[test]
fn the_words_say_only_what_a_bot_does() {
    let all: Vec<&str> = [
        Words::BotsSetupGreeting,
        Words::BotsSetupContextHint,
        Words::BotsSetupHint,
        Words::BotsSetupExampleDigest,
        Words::BotsSetupExampleTasks,
        Words::BotsSetupExampleSpecs,
        Words::BotsSetupExampleDigestText,
        Words::BotsSetupExampleTasksText,
        Words::BotsSetupExampleSpecsText,
    ]
    .into_iter()
    .map(|key| t(Lang::En, key))
    .collect();
    for said in &all {
        let lower = said.to_lowercase();
        for untrue in [
            "sees nothing else",
            "works out its setup",
            "every monday",
            "weekly",
            "when they're ready",
            "when i add a new note",
            "moves to ready",
            "apps",
            "folder",
        ] {
            assert!(!lower.contains(untrue), "{said:?} says {untrue:?}");
        }
    }
    let context = t(Lang::En, Words::BotsSetupContextHint);
    assert!(context.contains("already read every page"));
    assert!(context.contains("doesn't let it edit"));
    assert!(t(Lang::En, Words::BotsSetupRoleNote).contains("in place of what is there"));
}
