// SPDX-License-Identifier: MPL-2.0

//! **One Bot asks another, in the Bot's tab** (M6, piece U): the band over
//! the conversation driven through the tab, with fake owner replies on the
//! band's own link, and the settings' *May ask other bots*. Cohorts
//! `m6_ui_keyboard`, `m6_ui_request_card`, `m6_ui_child_header` and
//! `m6_ui_may_ask` (contract 10.3).

use super::*;
use crate::bots::children::{self, OpenRequest};
use koru_ai_core::MessageId;
use koru_ai_core::bots::{
    BotChildOrigin, BotChildOutcome, BotChildProgress, BotChildRequestCard, BotChildResult,
};
use koru_delegate_core::ChildRequestId;
use koru_delegate_core::children::{ChildClock, ChildRequestState};
use winit::keyboard::NamedKey;

struct Band {
    f: Fixture,
    commands: Receiver<Command>,
    answers: SyncSender<Answer>,
    chat: ChatId,
}

/// A Bot tab on one of its additional conversations, its Bot known, the
/// band's link held by the test. Shown once: the band has asked.
fn band() -> Band {
    let mut f = fixture();
    f.view.know_bot_for_test(f.bot.clone());
    let chat = ChatId::new();
    f.view.show_conversation(chat);
    let (link, commands, answers) = BotsLink::test_pair();
    f.view.children_with_link(link);
    f.view.shown_pass(None);
    Band {
        f,
        commands,
        answers,
        chat,
    }
}

impl Band {
    fn asked(&self) -> (u64, Option<u64>) {
        let (mut cards, mut origin) = (None, None);
        while let Ok(command) = self.commands.try_recv() {
            match command {
                Command::ChildRequests { seq, chat, .. } if chat == self.chat => cards = Some(seq),
                Command::ChildOrigin { seq, chat, .. } if chat == self.chat => origin = Some(seq),
                _ => panic!("the band reads this conversation's cards and origin only"),
            }
        }
        (cards.expect("the cards were read"), origin)
    }

    fn answer(&mut self, cards: Vec<BotChildRequestCard>, origin: Option<BotChildOrigin>) {
        let (seq, origin_seq) = self.asked();
        let bot = self.f.bot.id;
        self.answers
            .send(Answer::ChildRequests {
                seq,
                bot,
                chat: self.chat,
                result: Ok(cards),
            })
            .unwrap();
        self.answers
            .send(Answer::ChildOrigin {
                seq: origin_seq.expect("an additional conversation reads its origin"),
                bot,
                chat: self.chat,
                result: Ok(origin),
            })
            .unwrap();
        assert!(self.f.view.shown_pass(None), "the band changed");
        self.f.publishable("after the band's answer");
    }
}

fn card(name: &str) -> BotChildRequestCard {
    BotChildRequestCard {
        child_request_id: ChildRequestId::mint(),
        parent_assistant_message_id: MessageId::new(),
        recipient_bot_id: BotId::mint(),
        recipient_name: name.to_owned(),
        child_chat_id: ChatId::new(),
        state: ChildRequestState::Open,
        child: BotChildProgress::Running,
        outcome: None,
        clock: ChildClock::Running,
        result: BotChildResult::None,
        needs_you: false,
    }
}

fn origin() -> BotChildOrigin {
    BotChildOrigin {
        child_request_id: ChildRequestId::mint(),
        parent_bot_id: BotId::mint(),
        parent_name: "Planner".to_owned(),
        parent_chat_id: ChatId::new(),
        state: ChildRequestState::Settled,
        outcome: Some(BotChildOutcome::TimedOut),
    }
}

/// Every published control, depth first.
fn flat(controls: &[Control]) -> Vec<&Control> {
    let mut out = Vec::new();
    for control in controls {
        out.push(control);
        out.extend(flat(&control.children));
    }
    out
}

fn tab_key(view: &mut BotView, key: NamedKey) -> Option<bool> {
    view.region_key(
        &WinitKey::Named(key),
        None,
        winit::keyboard::ModifiersState::empty(),
    )
}

/// **The band sits over the conversation, and the conversation below it**:
/// its cards and origin line are drawn, published with their words, and the
/// chat is placed under them — never covered.
#[test]
fn m6_ui_request_card_the_band_stands_over_the_conversation_and_is_published() {
    let mut b = band();
    let scout = card("Scout");
    b.answer(vec![scout.clone()], Some(origin()));
    let (band, conversation) = b.f.view.with_frame(|_, frame| {
        (
            frame.node(children::BAND).expect("the band").rect,
            frame.node(CONVERSATION).expect("the conversation").rect,
        )
    });
    assert!(
        conversation.y >= band.y + band.h,
        "the chat is under the band"
    );
    let controls = b.f.view.controls();
    let published = flat(&controls);
    let labels: Vec<&str> = published.iter().map(|c| c.label.as_str()).collect();
    for words in [
        "Requests to other bots",
        "Request to Scout",
        "Asked Scout",
        "Scout is working",
        "Open conversation",
        "Asked by Planner",
        "Open Planner's conversation",
        "Planner stopped waiting after 15 minutes. This work stays here.",
    ] {
        assert!(labels.contains(&words), "{words} in {labels:?}");
    }
    let open = published
        .iter()
        .find(|c| c.key == children::open_key(&scout))
        .expect("the card's link");
    assert_eq!(open.role, ControlRole::Button);
    // The waiting line names this tab's Bot (the fixture's, *Scout*).
    let queued = koru_i18n::fill(t(Lang::En, Words::BotsWaitingQueued), &["Scout"]);
    assert!(labels.contains(&queued.as_str()), "{labels:?}");
}

/// Cohort `m6_ui_keyboard`: **every link of the band is a keyboard stop**
/// after the header's, Enter on one asks the window for that conversation,
/// and the keyboard stays on the link it activated — so coming back to the
/// tab finds it there (contract 7.5).
#[test]
fn m6_ui_keyboard_the_bands_links_are_stops_and_keep_the_focus() {
    let mut b = band();
    let scout = card("Scout");
    let asked = origin();
    b.answer(vec![scout.clone()], Some(asked.clone()));
    let order = b.f.view.focus_order();
    let at = |key: Key| order.iter().position(|k| *k == key);
    let settings = at(region::SETTINGS_BUTTON).expect("the header's Settings");
    let parent = at(children::OPEN_PARENT).expect("the origin's link");
    let open = at(children::open_key(&scout)).expect("the card's link");
    assert!(settings < parent && parent < open, "{order:?}");

    assert!(b.f.view.focus_header());
    while b.f.view.focus != Some(children::open_key(&scout)) {
        assert_eq!(tab_key(&mut b.f.view, NamedKey::Tab), Some(true));
    }
    assert_eq!(tab_key(&mut b.f.view, NamedKey::Enter), Some(true));
    assert_eq!(
        b.f.view.take_open_request(),
        Some(OpenRequest {
            bot: scout.recipient_bot_id,
            name: "Scout".to_owned(),
            chat: scout.child_chat_id,
        })
    );
    assert_eq!(b.f.view.take_open_request(), None, "taken once");
    assert_eq!(
        b.f.view.focus,
        Some(children::open_key(&scout)),
        "focus stays"
    );

    // Shift+Tab back to the origin's link; Space opens the asking Bot's.
    while b.f.view.focus != Some(children::OPEN_PARENT) {
        assert_eq!(
            b.f.view.region_key(
                &WinitKey::Named(NamedKey::Tab),
                None,
                winit::keyboard::ModifiersState::SHIFT,
            ),
            Some(true)
        );
    }
    assert_eq!(tab_key(&mut b.f.view, NamedKey::Space), Some(true));
    assert_eq!(
        b.f.view.take_open_request().map(|request| request.chat),
        Some(asked.parent_chat_id)
    );

    // An assistive technology's click asks for the same thing.
    assert!(
        b.f.view
            .accessibility_action(children::open_key(&scout), true)
    );
    assert_eq!(
        b.f.view.take_open_request().map(|request| request.chat),
        Some(scout.child_chat_id)
    );
}

/// **Hidden, the band asks nothing; shown again it reads once**; a tab whose
/// Bot is in Trash has no band.
#[test]
fn m6_ui_request_card_a_hidden_tab_reads_nothing() {
    let mut b = band();
    b.answer(vec![card("Scout")], None);
    b.f.view.hidden();
    assert!(b.commands.try_recv().is_err());
    assert_eq!(
        b.f.view.children().deadline(std::time::Instant::now()),
        None,
        "no clock while hidden"
    );
    b.f.view.shown_pass(None);
    b.asked();
}

/// Cohort `m6_ui_may_ask`: **the switch is in the settings' access card**,
/// published as a toggle in its state, with what it allows. A build that
/// keeps asking closed also says that it is kept for later; this build asks
/// (the product owner opened it on 2026-11-17) and does not say so. Pressing
/// it is an access edit, saved with the card's *Save*.
#[test]
fn m6_ui_may_ask_the_switch_is_a_toggle_in_the_access_card() {
    let mut f = fixture();
    f.view.know_bot_for_test(f.bot.clone());
    f.view.set_settings_open(true);
    let controls = f.view.controls();
    let published = flat(&controls);
    let switch = published
        .iter()
        .find(|c| c.key == region::MAY_ASK)
        .expect("the switch");
    assert_eq!(switch.role, ControlRole::Toggle);
    assert_eq!(switch.selected, Some(true));
    assert_eq!(switch.label, "May ask other bots");
    let labels: Vec<&str> = published.iter().map(|c| c.label.as_str()).collect();
    assert!(labels.contains(&t(Lang::En, Words::BotsMayAskHelp)));
    // The pin of the app's half of the switch: open, with the gateway's.
    assert!(children::ASKING_OPEN);
    assert_eq!(
        labels.contains(&t(Lang::En, Words::BotsMayAskNotYet)),
        !children::ASKING_OPEN,
        "the sentence is true for this build"
    );
    assert!(
        !labels.contains(&t(Lang::En, Words::BotsMayAskNotYet)),
        "a build that asks does not say asking is kept for later"
    );
    assert!(f.view.focus_order().contains(&region::MAY_ASK));
    f.view.focus = Some(region::MAY_ASK);
    assert_eq!(tab_key(&mut f.view, NamedKey::Enter), Some(true));
    assert!(!f.view.settings().may_ask_shown());
    let controls = f.view.controls();
    let switch = flat(&controls)
        .into_iter()
        .find(|c| c.key == region::MAY_ASK)
        .cloned()
        .expect("the switch");
    assert_eq!(switch.selected, Some(false));
    f.publishable("the settings with the switch");
}
