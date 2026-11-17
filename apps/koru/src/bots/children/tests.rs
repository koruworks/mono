// SPDX-License-Identifier: MPL-2.0

//! Piece U's cohorts for the band (contract 10.3): `m6_ui_request_card`,
//! `m6_ui_child_header`, `m6_ui_queued_message` and `m6_i18n_complete`.
//! Host-fed, with plain data: no Store, no owner, no thread — the link is a
//! test pair this file answers.

use std::sync::mpsc::{Receiver, SyncSender};
use std::time::{Duration, Instant};

use super::*;
use koru_ai_core::MessageId;
use koru_delegate_core::ChildRequestId;
use koru_draw::el::{FrameCx, TextCache};
use koru_draw::geometry::Rect;

fn card(name: &str, ask: MessageId) -> BotChildRequestCard {
    BotChildRequestCard {
        child_request_id: ChildRequestId::mint(),
        parent_assistant_message_id: ask,
        recipient_bot_id: BotId::mint(),
        recipient_name: name.to_owned(),
        child_chat_id: ChatId::new(),
        state: ChildRequestState::Open,
        child: BotChildProgress::Queued,
        outcome: None,
        clock: ChildClock::Running,
        result: BotChildResult::None,
        needs_you: false,
    }
}

fn settled(name: &str, outcome: BotChildOutcome, result: BotChildResult) -> BotChildRequestCard {
    BotChildRequestCard {
        state: ChildRequestState::Settled,
        child: BotChildProgress::Finished,
        outcome: Some(outcome),
        clock: ChildClock::Stopped,
        result,
        ..card(name, MessageId::new())
    }
}

fn origin(state: ChildRequestState, outcome: Option<BotChildOutcome>) -> BotChildOrigin {
    BotChildOrigin {
        child_request_id: ChildRequestId::mint(),
        parent_bot_id: BotId::mint(),
        parent_name: "Planner".to_owned(),
        parent_chat_id: ChatId::new(),
        state,
        outcome,
    }
}

fn status(card: &BotChildRequestCard) -> (String, StatusTone) {
    status_words(card, Lang::En)
}

fn en(key: Words) -> &'static str {
    t(Lang::En, key)
}

/// **Every row of contract 7.2's table says its sentence**, each in the
/// tone it reads in: the open states plain and Needs you with the attention
/// treatment, every ending quiet. The asked Bot is named where the table
/// names it, and nowhere else.
#[test]
fn m6_ui_request_card_every_state_of_the_table_has_its_line() {
    let ask = MessageId::new();
    let mut queued = card("Scout", ask);
    assert_eq!(
        status(&queued),
        ("Waiting in Scout's queue".to_owned(), StatusTone::Plain)
    );
    queued.child = BotChildProgress::Running;
    assert_eq!(
        status(&queued),
        ("Scout is working".to_owned(), StatusTone::Plain)
    );
    queued.child = BotChildProgress::NeedsYou;
    assert_eq!(
        status(&queued),
        ("Scout needs you".to_owned(), StatusTone::Attention)
    );
    // The worker's Needs-you flag is the card's own (piece G: the ledger's
    // `child` never says needs_you; W's card does).
    queued.child = BotChildProgress::Running;
    queued.needs_you = true;
    assert_eq!(status(&queued).1, StatusTone::Attention);

    let table = [
        (
            settled(
                "Scout",
                BotChildOutcome::Completed,
                BotChildResult::Saved { pages: 2 },
            ),
            en(Words::BotsChildSaved).to_owned(),
        ),
        (
            settled(
                "Scout",
                BotChildOutcome::Completed,
                BotChildResult::NoPageSaved,
            ),
            en(Words::BotsChildNoPage).to_owned(),
        ),
        (
            settled("Scout", BotChildOutcome::Failed, BotChildResult::None),
            "Scout couldn't finish".to_owned(),
        ),
        (
            settled("Scout", BotChildOutcome::NeededYou, BotChildResult::None),
            "Scout stopped because it needed you".to_owned(),
        ),
        (
            settled("Scout", BotChildOutcome::Canceled, BotChildResult::None),
            "You stopped Scout".to_owned(),
        ),
        (
            settled("Scout", BotChildOutcome::TimedOut, BotChildResult::None),
            "Scout didn't finish in 15 minutes".to_owned(),
        ),
        (
            settled(
                "Scout",
                BotChildOutcome::ParentStopped,
                BotChildResult::None,
            ),
            en(Words::BotsChildParentEnded).to_owned(),
        ),
    ];
    for (card, words) in table {
        assert_eq!(status(&card), (words, StatusTone::Settled), "{card:?}");
    }
}

/// **A settled request is never guessed at.** One whose ending the owner
/// has not read yet says it is being checked — not *failed*, not *saved* —
/// and a completed request that names no page count reads as no page saved,
/// never as a saved result.
#[test]
fn m6_ui_request_card_an_ending_not_read_yet_is_checked_not_guessed() {
    let mut pending = settled("Scout", BotChildOutcome::Completed, BotChildResult::None);
    assert_eq!(status(&pending).0, en(Words::BotsChildNoPage));
    pending.result = BotChildResult::Saved { pages: 0 };
    assert_eq!(status(&pending).0, en(Words::BotsChildNoPage));
    pending.outcome = None;
    let (words, tone) = status(&pending);
    assert_eq!(words, en(Words::BotsChildChecking));
    assert_eq!(tone, StatusTone::Plain);
    assert_ne!(words, "Scout couldn't finish");
}

/// **Paused is said while the asking Bot is paused** (decision 14: its
/// wait has no time limit then), and only on an open request.
#[test]
fn m6_ui_request_card_a_paused_clock_says_paused_on_an_open_request() {
    let mut open = card("Scout", MessageId::new());
    assert_eq!(note_words(&open, Lang::En), None);
    open.clock = ChildClock::Paused;
    assert_eq!(
        note_words(&open, Lang::En).as_deref(),
        Some(en(Words::BotsPaused))
    );
    let mut ended = settled("Scout", BotChildOutcome::TimedOut, BotChildResult::None);
    ended.clock = ChildClock::Paused;
    assert_eq!(note_words(&ended, Lang::En), None);
}

/// **The band draws the latest ask's cards and any still open**, in the
/// owner's order — never every card a long conversation ever made.
#[test]
fn m6_ui_request_card_only_the_latest_ask_and_open_requests_are_drawn() {
    let (early, late) = (MessageId::new(), MessageId::new());
    let old_done = BotChildRequestCard {
        parent_assistant_message_id: early,
        ..settled(
            "Old",
            BotChildOutcome::Completed,
            BotChildResult::NoPageSaved,
        )
    };
    let old_open = card("Slow", early);
    let new_a = card("Scout", late);
    let new_b = BotChildRequestCard {
        parent_assistant_message_id: late,
        ..settled("Ledger", BotChildOutcome::Failed, BotChildResult::None)
    };
    let mut children = Children::default();
    children.know_for_test(
        vec![
            old_done.clone(),
            old_open.clone(),
            new_a.clone(),
            new_b.clone(),
        ],
        None,
    );
    let shown: Vec<_> = children
        .shown_cards()
        .into_iter()
        .map(|card| card.recipient_name.clone())
        .collect();
    assert_eq!(shown, ["Slow", "Scout", "Ledger"]);
    assert!(children.waiting());
}

fn button(name: &'static str, key: Key, label: String) -> El {
    text(label)
        .named(name)
        .id(key)
        .focusable()
        .semantics(Role::Button)
        .name_text(String::new())
}

fn lay(children: &Children) -> Option<koru_draw::el::Frame> {
    let tree = children.band(&mut button)?;
    crate::input_engine::with(|engine| {
        Some(tree.layout(
            engine,
            &mut TextCache::default(),
            Rect::new(0.0, 0.0, 480.0, 900.0),
            FrameCx::root(),
        ))
    })
}

/// **A card is the shared status card, fed plain words**: its group named
/// *Request to {bot}*, *Asked {bot}*, the status line, *Paused* while held,
/// and *Open conversation* — each at its own key, which the tab publishes
/// and the words read back from.
#[test]
fn m6_ui_request_card_is_drawn_with_its_words_at_its_keys() {
    let mut open = card("Scout", MessageId::new());
    open.clock = ChildClock::Paused;
    let mut children = Children::default();
    children.set_words(Lang::En, "Planner");
    children.know_for_test(vec![open.clone()], None);
    let frame = lay(&children).expect("a band");
    assert!(frame.node(BAND).is_some());
    for part in [CARD_PART, TITLE_PART, STATUS_PART, NOTE_PART, OPEN_PART] {
        assert!(frame.node(card_key(&open, part)).is_some(), "{part}");
        assert!(is_band_key(card_key(&open, part)));
    }
    assert_eq!(
        children.line_words(card_key(&open, TITLE_PART)).as_deref(),
        Some("Asked Scout")
    );
    assert_eq!(
        children.line_words(card_key(&open, STATUS_PART)).as_deref(),
        Some("Waiting in Scout's queue")
    );
    assert_eq!(
        children.line_words(card_key(&open, NOTE_PART)).as_deref(),
        Some("Paused")
    );
    let group = frame.node(card_key(&open, CARD_PART)).expect("the card");
    assert!(
        matches!(&group.label, Some(koru_draw::el::Name::Text(name)) if name == "Request to Scout")
    );
    // *Open conversation* leads to the asked Bot's own conversation.
    assert_eq!(
        children.activate(open_key(&open)),
        Some(OpenRequest {
            bot: open.recipient_bot_id,
            name: "Scout".to_owned(),
            chat: open.child_chat_id,
        })
    );
    assert!(Children::is_link(open_key(&open)));
    assert!(!Children::is_link(card_key(&open, STATUS_PART)));
}

/// **The band says nothing when there is nothing to say**: no cards, no
/// origin, no band — an ordinary conversation is unchanged.
#[test]
fn m6_ui_request_card_an_ordinary_conversation_has_no_band() {
    let children = Children::default();
    assert!(!children.shows());
    assert!(lay(&children).is_none());
}

/// **The origin line names who asked and leads back** (contract 7.3): *Asked
/// by {bot}*, *Open {bot}'s conversation* to the asking Bot's conversation,
/// and no closing line while the request is open.
#[test]
fn m6_ui_child_header_names_the_asker_and_leads_back_to_it() {
    let asked = origin(ChildRequestState::Open, None);
    let mut children = Children::default();
    children.set_words(Lang::En, "Scout");
    children.know_for_test(Vec::new(), Some(asked.clone()));
    let frame = lay(&children).expect("a band");
    assert!(frame.node(ORIGIN).is_some());
    assert!(frame.node(OPEN_PARENT).is_some());
    assert!(frame.node(CLOSED).is_none(), "nothing closed yet");
    assert!(
        frame.node(QUEUED).is_none(),
        "a child conversation is not the one waiting"
    );
    assert_eq!(
        children.line_words(ORIGIN).as_deref(),
        Some("Asked by Planner")
    );
    assert_eq!(
        children.activate(OPEN_PARENT),
        Some(OpenRequest {
            bot: asked.parent_bot_id,
            name: "Planner".to_owned(),
            chat: asked.parent_chat_id,
        })
    );
}

/// **The closing lines** (contract 7.3): after the 15 minutes and after the
/// asking task ended, each saying the work stays; every other ending, and an
/// open request, has none.
#[test]
fn m6_ui_child_header_closes_only_a_request_settled_without_a_handoff() {
    let timed_out = origin(ChildRequestState::Settled, Some(BotChildOutcome::TimedOut));
    assert_eq!(
        closing_words(&timed_out, Lang::En).as_deref(),
        Some("Planner stopped waiting after 15 minutes. This work stays here.")
    );
    let ended = origin(
        ChildRequestState::Settled,
        Some(BotChildOutcome::ParentStopped),
    );
    assert_eq!(
        closing_words(&ended, Lang::En).as_deref(),
        Some("Planner's task ended, so this request was canceled. This work stays here.")
    );
    for outcome in [
        BotChildOutcome::Completed,
        BotChildOutcome::Failed,
        BotChildOutcome::NeededYou,
        BotChildOutcome::Canceled,
    ] {
        assert_eq!(
            closing_words(&origin(ChildRequestState::Settled, Some(outcome)), Lang::En),
            None,
            "{outcome:?}"
        );
    }
    assert_eq!(
        closing_words(
            &origin(ChildRequestState::Open, Some(BotChildOutcome::TimedOut)),
            Lang::En
        ),
        None
    );
    let mut children = Children::default();
    children.know_for_test(Vec::new(), Some(timed_out));
    let frame = lay(&children).expect("a band");
    assert!(frame.node(CLOSED).is_some());
}

/// **While a request is open the conversation says a message waits**
/// (contract 7.4), naming the Bot that waits; once every request is settled
/// it says nothing.
#[test]
fn m6_ui_queued_message_is_said_while_a_request_is_open() {
    let mut children = Children::default();
    children.set_words(Lang::En, "Planner");
    let open = card("Scout", MessageId::new());
    children.know_for_test(vec![open.clone()], None);
    let frame = lay(&children).expect("a band");
    assert!(frame.node(QUEUED).is_some());
    let said = children.line_words(QUEUED).expect("the line");
    assert!(
        said.starts_with("Planner is waiting for another bot."),
        "{said}"
    );
    children.know_for_test(
        vec![settled(
            "Scout",
            BotChildOutcome::Completed,
            BotChildResult::NoPageSaved,
        )],
        None,
    );
    assert!(!children.waiting());
    let frame = lay(&children).expect("a band");
    assert!(frame.node(QUEUED).is_none());
    assert_eq!(children.line_words(QUEUED), None);
}

// ------------------------------------------------------------ the reads

struct Fixture {
    children: Children,
    commands: Receiver<Command>,
    answers: SyncSender<Answer>,
    bot: BotId,
    chat: ChatId,
}

/// The band of a shown additional conversation, over a link this test
/// answers.
fn fixture(main: bool) -> Fixture {
    let (link, commands, answers) = BotsLink::test_pair();
    let mut children = Children::with_link(link);
    let (bot, chat) = (BotId::mint(), ChatId::new());
    children.set_subject(bot, chat, if main { chat } else { ChatId::new() });
    children.set_words(Lang::En, "Planner");
    children.show(true);
    Fixture {
        children,
        commands,
        answers,
        bot,
        chat,
    }
}

impl Fixture {
    /// The reads just sent: the cards' sequence, and the origin's when one
    /// was asked.
    fn asked(&self) -> (u64, Option<u64>) {
        let mut cards = None;
        let mut origin = None;
        while let Ok(command) = self.commands.try_recv() {
            match command {
                Command::ChildRequests { seq, bot, chat } => {
                    assert_eq!((bot, chat), (self.bot, self.chat));
                    cards = Some(seq);
                }
                Command::ChildOrigin { seq, bot, chat } => {
                    assert_eq!((bot, chat), (self.bot, self.chat));
                    origin = Some(seq);
                }
                _ => panic!("the band reads its cards and its origin only"),
            }
        }
        (cards.expect("the cards were read"), origin)
    }

    fn cards(
        &mut self,
        seq: u64,
        result: Result<Vec<BotChildRequestCard>, Failure>,
        now: Instant,
    ) -> bool {
        self.answers
            .send(Answer::ChildRequests {
                seq,
                bot: self.bot,
                chat: self.chat,
                result,
            })
            .unwrap();
        self.children.pass(now)
    }

    fn origin(
        &mut self,
        seq: u64,
        result: Result<Option<BotChildOrigin>, Failure>,
        now: Instant,
    ) -> bool {
        self.answers
            .send(Answer::ChildOrigin {
                seq,
                bot: self.bot,
                chat: self.chat,
                result,
            })
            .unwrap();
        self.children.pass(now)
    }
}

/// **Shown, it reads once; an open request is read again on its clock; a
/// settled one is not.** The reads go to the link — never the window
/// thread — and only while the tab is shown.
#[test]
fn m6_ui_request_card_reads_while_shown_and_again_only_while_open() {
    let mut f = fixture(true);
    let now = Instant::now();
    f.children.pass(now);
    let (seq, origin) = f.asked();
    assert_eq!(origin, None, "the main conversation has no origin to read");
    assert!(
        !f.children.pass(now),
        "nothing more is asked while it is out"
    );
    assert!(f.commands.try_recv().is_err());

    let open = card("Scout", MessageId::new());
    assert!(f.cards(seq, Ok(vec![open.clone()]), now));
    assert_eq!(f.children.cards(), [open.clone()]);
    assert_eq!(f.children.deadline(now), now.checked_add(OPEN_POLL));
    f.children.pass(now + OPEN_POLL);
    let (again, _) = f.asked();

    let done = BotChildRequestCard {
        state: ChildRequestState::Settled,
        outcome: Some(BotChildOutcome::Completed),
        result: BotChildResult::NoPageSaved,
        ..open
    };
    assert!(f.cards(again, Ok(vec![done]), now + OPEN_POLL));
    assert_eq!(
        f.children.deadline(now + OPEN_POLL),
        None,
        "nothing open, no clock"
    );
    f.children.pass(now + OPEN_POLL * 10);
    assert!(
        f.commands.try_recv().is_err(),
        "a settled request is not polled"
    );

    // Hidden: nothing is asked and no clock runs; shown again, it reads once.
    f.children.show(false);
    assert_eq!(f.children.deadline(now), None);
    f.children.owe_read();
    assert!(!f.children.pass(now));
    assert!(f.commands.try_recv().is_err());
    f.children.show(true);
    f.children.pass(now);
    f.asked();
}

/// **`bot_unavailable` is a calm retry, never a failure** (piece W: the
/// delegate did not answer the worker). The cards shown stay, a quiet line
/// says they are being checked again, and the read is asked again on a
/// bounded clock; the next answer takes the line away. A refusal that would
/// only repeat is not asked again.
#[test]
fn m6_ui_request_card_an_unavailable_owner_is_retried_calmly() {
    let mut f = fixture(true);
    let now = Instant::now();
    f.children.pass(now);
    let (seq, _) = f.asked();
    let open = card("Scout", MessageId::new());
    f.cards(seq, Ok(vec![open.clone()]), now);
    f.children.pass(now + OPEN_POLL);
    let (seq, _) = f.asked();

    let unavailable = Failure::of(crate::bots::link::Said::new("bot_unavailable", true));
    assert!(unavailable.again(), "retryable, as the link classifies it");
    assert!(f.cards(seq, Err(unavailable), now + OPEN_POLL));
    assert!(f.children.stale());
    assert_eq!(f.children.cards(), [open.clone()], "what was shown stays");
    let frame = lay(&f.children).expect("a band");
    assert!(frame.node(STALE).is_some());
    let said = f.children.line_words(STALE).expect("the line");
    assert!(!said.to_lowercase().contains("fail"), "{said}");
    for card in f.children.cards() {
        assert_ne!(status(card).0, "Scout couldn't finish");
    }
    let retry = f.children.deadline(now + OPEN_POLL).expect("a retry");
    assert!(retry <= now + OPEN_POLL + Duration::from_secs(1));
    f.children.pass(retry);
    let (seq, _) = f.asked();
    assert!(f.cards(seq, Ok(vec![open.clone()]), retry));
    assert!(!f.children.stale(), "answered: the line goes");

    // A lost connection is the same calm retry.
    f.children.pass(retry + OPEN_POLL);
    let (seq, _) = f.asked();
    f.cards(seq, Err(Failure::lost()), retry + OPEN_POLL);
    assert!(f.children.stale());

    // A refusal that would only repeat: kept as shown, not asked again.
    let mut g = fixture(true);
    g.children.pass(now);
    let (seq, _) = g.asked();
    g.cards(seq, Err(Failure::Refused("not_found".into())), now);
    assert!(!g.children.stale(), "nothing was shown to be stale");
    assert_eq!(g.children.deadline(now), None);
}

/// **An answer about another conversation, or an older read, is never this
/// one's**: changing the conversation drops what was shown and reads again.
#[test]
fn m6_ui_request_card_an_answer_for_another_conversation_is_ignored() {
    let mut f = fixture(true);
    let now = Instant::now();
    f.children.pass(now);
    let (seq, _) = f.asked();
    let other = ChatId::new();
    assert!(f.children.set_subject(f.bot, other, other));
    assert!(!f.cards(seq, Ok(vec![card("Scout", MessageId::new())]), now));
    assert!(f.children.cards().is_empty());
    f.chat = other;
    f.children.pass(now);
    let (fresh, _) = f.asked();
    assert_ne!(fresh, seq);
}

/// **The child conversation's origin is read and handed on once**, for the
/// tab's conversation list to label the row; a settled origin with a closing
/// line is not read again.
#[test]
fn m6_ui_child_header_the_origin_is_read_and_labels_the_row_once() {
    let mut f = fixture(false);
    let now = Instant::now();
    f.children.pass(now);
    let (seq, origin_seq) = f.asked();
    f.cards(seq, Ok(Vec::new()), now);
    let asked = origin(ChildRequestState::Open, None);
    assert!(f.origin(origin_seq.expect("asked"), Ok(Some(asked.clone())), now));
    assert_eq!(f.children.origin(), Some(&asked));
    assert_eq!(
        f.children.take_origin_news(),
        Some((f.chat, "Planner".to_owned()))
    );
    assert_eq!(f.children.take_origin_news(), None, "taken once");
    assert_eq!(
        f.children.deadline(now),
        now.checked_add(OPEN_POLL),
        "an open request is read again"
    );
}

/// Cohort `m6_i18n_complete`: **every word the band, the roster and the
/// settings say is in all eight languages**, with the English placeholders,
/// and no other language is English copied.
#[test]
fn m6_i18n_complete_every_new_word_is_in_all_eight_languages() {
    let keys = [
        Words::BotsChildAsked,
        Words::BotsChildQueued,
        Words::BotsChildWorking,
        Words::BotsChildNeedsYou,
        Words::BotsChildSaved,
        Words::BotsChildNoPage,
        Words::BotsChildFailed,
        Words::BotsChildNeededYou,
        Words::BotsChildStoppedByYou,
        Words::BotsChildTimedOut,
        Words::BotsChildParentEnded,
        Words::BotsChildOpenConversation,
        Words::BotsChildCardName,
        Words::BotsChildChecking,
        Words::BotsChildStale,
        Words::BotsChildAskedBy,
        Words::BotsChildOpenParent,
        Words::BotsChildClosedTimedOut,
        Words::BotsChildClosedParentEnded,
        Words::BotsWaitingForBot,
        Words::BotsWaitingForBots,
        Words::BotsWaitingForAnother,
        Words::BotsWaitingPaused,
        Words::BotsWaitingPausedBots,
        Words::BotsRowWaiting,
        Words::BotsWaitingQueued,
        Words::BotsMayAsk,
        Words::BotsMayAskHelp,
        Words::BotsMayAskNotYet,
        Words::RunNeedsOtherBot,
        Words::RunStepWaitingForBot,
        Words::BotsChildRequests,
    ];
    for key in keys {
        let english = t(Lang::En, key);
        let holes = english.matches("{}").count();
        for lang in Lang::ALL {
            let words = t(lang, key);
            assert!(!words.trim().is_empty(), "{key:?} in {lang:?}");
            assert_eq!(words.matches("{}").count(), holes, "{key:?} in {lang:?}");
            if lang != Lang::En {
                assert_ne!(words, english, "{key:?} copied into {lang:?}");
            }
        }
    }
}
