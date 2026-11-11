// SPDX-License-Identifier: MPL-2.0

use std::sync::mpsc::{Receiver, SyncSender};

use super::*;
use crate::bots::tests::bot;

struct Fixture {
    view: BotView,
    commands: Receiver<Command>,
    answers: SyncSender<Answer>,
    bot: Bot,
}

/// A restored Bot tab over a link the test answers, not shown.
fn fixture() -> Fixture {
    let (link, commands, answers) = BotsLink::test_pair();
    let bot = bot(BotId::mint(), WorkspaceId::new(), "Scout", true);
    let mut view = BotView::with_link(bot.id, bot.main_chat, link);
    view.set_viewport(640.0, 480.0);
    Fixture {
        view,
        commands,
        answers,
        bot,
    }
}

impl Fixture {
    /// The one Read the tab just sent: its sequence number.
    fn asked(&self) -> u64 {
        match self.commands.try_recv().expect("a request was sent") {
            Command::Read { seq, bot } => {
                assert_eq!(bot, self.bot.id, "about this tab's Bot");
                seq
            }
            Command::List { .. } => panic!("a Bot tab only reads its Bot"),
        }
    }

    fn answer(&mut self, seq: u64, bot: BotId, result: Result<Bot, Failure>) -> bool {
        self.answers
            .send(Answer::Read { seq, bot, result })
            .expect("the tab's end is open");
        self.view.poll()
    }
}

/// **A Bot tab behind another sends zero commands and does zero layouts.**
/// Sizing it, asking its deadline and its maintenance — everything the
/// window does to a tab it is not showing — costs nothing; the first
/// showing asks once.
#[test]
fn a_hidden_bot_tab_sends_zero_commands_and_does_zero_layouts() {
    let mut f = fixture();
    f.view.set_viewport(800.0, 600.0);
    assert_eq!(f.view.maintenance_deadline(), None);
    let mut ws = koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t")
        .expect("a document");
    assert!(!View::maintain(
        &mut f.view,
        &mut ws,
        std::time::Instant::now()
    ));
    assert_eq!(f.view.work(), Work::default());
    assert!(f.commands.try_recv().is_err(), "nothing was asked");
    assert_eq!(f.view.load(), Load::Cold);
    assert!(f.view.conversation().is_none(), "no conversation started");

    assert!(f.view.shown_pass(None), "shown: it asks");
    f.asked();
    assert_eq!(f.view.work().commands, 1);
    assert_eq!(f.view.load(), Load::Loading);
    assert!(!f.view.shown_pass(None), "and only once");
    assert!(f.commands.try_recv().is_err());
}

/// **A stale Bot read is ignored**: an answer to an older request, or about
/// another Bot, never replaces what the tab knows. The latest answer binds
/// the conversation to the Bot's home and agent; a later failure keeps what
/// was shown and says the service is not answering over it.
#[test]
fn a_stale_bot_read_is_ignored_and_a_failed_one_keeps_what_was_shown() {
    let mut f = fixture();
    assert!(f.view.shown_pass(None));
    let seq = f.asked();

    let mut renamed = f.bot.clone();
    renamed.config.name = "Old name".into();
    assert!(!f.answer(seq.wrapping_sub(1), f.bot.id, Ok(renamed)));
    assert!(!f.answer(seq, BotId::mint(), Ok(f.bot.clone())));
    assert!(
        f.view.facts().is_none(),
        "neither was this request's answer"
    );

    assert!(f.answer(seq, f.bot.id, Ok(f.bot.clone())));
    assert_eq!(f.view.load(), Load::Ready);
    assert_eq!(f.view.label(), "Scout");
    let owned = f
        .view
        .conversation()
        .and_then(AiChatView::owned_conversation)
        .expect("bound to the Bot");
    assert_eq!(owned.chat, f.bot.main_chat);
    assert_eq!(owned.destination, f.bot.home_workspace);
    assert_eq!(owned.provider, f.bot.config.model.provider);
    assert_eq!(owned.offers, BOT_OFFERS);

    // Asked again; the answer is a failure.
    assert!(f.view.ask());
    let again = f.asked();
    assert!(f.answer(again, f.bot.id, Err(Failure::Unreachable)));
    assert_eq!(f.view.load(), Load::Unavailable);
    assert_eq!(
        f.view.facts().map(|facts| facts.name.as_str()),
        Some("Scout"),
        "a failed read keeps what was shown"
    );
    assert!(f.view.conversation().is_some(), "and the conversation");
}

/// **The header says what the service said and offers nothing to press**:
/// the name, *Paused*, where the work goes and the home-only limit, in that
/// nesting under the tab; no element in it is a pointer target. While the
/// Bot is being read it says so, and claims no Bot.
#[test]
fn the_header_is_words_in_its_tree_and_no_control() {
    let mut f = fixture();
    f.view.shown_pass(None);
    let words: Vec<String> = f.view.header_lines().into_iter().map(|l| l.2).collect();
    assert_eq!(
        words,
        [t(f.view.lang, Words::BotsBotLoading)],
        "loading, no Bot"
    );
    let seq = f.asked();
    f.answer(seq, f.bot.id, Ok(f.bot.clone()));
    f.view.set_home_name(Some("Research".into()));

    let keys: Vec<Key> = f.view.header_lines().into_iter().map(|l| l.0).collect();
    assert_eq!(keys, [NAME, PAUSED, SAVES, LIMIT]);
    assert_eq!(
        f.view.saves_in(),
        Some(koru_i18n::fill(
            t(f.view.lang, Words::BotsSavesTo),
            &["Research"]
        ))
    );
    f.view.with_frame(|_, frame| {
        let header = frame.node(HEADER).expect("a header").rect;
        let conversation = frame.node(CONVERSATION).expect("a conversation").rect;
        assert!(header.h > 0.0);
        assert!(
            conversation.y >= header.y + header.h - 0.5,
            "the conversation sits below the header"
        );
        for key in [NAME, PAUSED, SAVES, LIMIT] {
            let rect = frame.node(key).expect("every line laid out").rect;
            assert!(
                rect.y >= header.y && rect.y + rect.h <= header.y + header.h + 0.5,
                "{key:?} nested in the header"
            );
            assert_eq!(
                frame.hit_at(rect.x + rect.w * 0.5, rect.y + rect.h * 0.5),
                None,
                "no control under {key:?}"
            );
        }
        assert!(
            frame.named("Bot tab").next().is_some()
                && frame.named("Bot header").next().is_some()
                && frame.named("Bot conversation").next().is_some(),
            "the tab, its header and its conversation are named elements"
        );
    });
}

/// The conversation is placed where the frame puts it, so a press on the
/// header never reaches the chat and a press below it does — the chat
/// keeps the tab's coordinates, offset by the header.
#[test]
fn the_conversation_sits_below_the_header_in_the_tabs_coordinates() {
    let mut f = fixture();
    f.view.shown_pass(None);
    let seq = f.asked();
    f.answer(seq, f.bot.id, Ok(f.bot.clone()));
    let (header, conversation) = f.view.with_frame(|_, frame| {
        (
            frame.node(HEADER).unwrap().rect,
            f.view.conversation_area(frame),
        )
    });
    assert!(!f.view.on_conversation(header.y + 1.0));
    assert!(f.view.on_conversation(conversation.y + 1.0));
    assert!(f.view.place_conversation(conversation));
    assert!(!f.view.place_conversation(conversation), "placed once");
}

/// **A Bot's conversation offers local path references and nothing else
/// the owner refuses.** The Bot owner delivers a file or folder attached by
/// its path as an ordinary chat's (QUESTIONS O30), so the tab admits that
/// choice; rename, pin, privacy, the chat's own working folder and its
/// workspace access stay absent. What each offer does to the surface — the
/// `+` menu's *Add folder*, the stored-or-reference choice for a file — is
/// pinned beside the seam in `koru-ai-ui`'s `owned/tests.rs`.
#[test]
fn a_bot_tab_offers_path_references_and_nothing_else_the_owner_refuses() {
    let bot = bot(BotId::mint(), WorkspaceId::new(), "Scout", false);
    let roster = crate::bots::roster::RosterBot::from(bot);
    let view = BotView::opened(&roster);
    let offers = view
        .conversation()
        .and_then(AiChatView::owned_conversation)
        .expect("bound to the Bot")
        .offers;
    assert!(offers.path_references, "the reference choice is offered");
    assert_eq!(
        offers,
        koru_ai_ui::OwnedOffers {
            path_references: true,
            ..koru_ai_ui::OwnedOffers::default()
        },
        "and nothing else beyond the conversation"
    );
}

/// Every language names the header's words.
#[test]
fn the_header_words_exist_in_every_language() {
    for lang in Lang::ALL {
        for key in [
            Words::BotsTabUnnamed,
            Words::BotsBotLoading,
            Words::BotsBotUnavailable,
            Words::BotsSavesTo,
            Words::BotsHomeOnly,
            Words::BotsHomeWorkspace,
        ] {
            assert!(!t(lang, key).trim().is_empty(), "{lang:?} {key:?}");
        }
        assert!(t(lang, Words::BotsSavesTo).contains("{}"), "{lang:?}");
    }
}
