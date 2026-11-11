// SPDX-License-Identifier: MPL-2.0

use std::sync::mpsc::{Receiver, SyncSender};

use super::*;
use crate::bots::tests::bot;
use koru_draw::el::{Name, SemNode};

fn area() -> Rect {
    Rect::new(
        0.0,
        koru_draw::theme::CAPTION_H,
        koru_draw::theme::SIDEBAR_W,
        600.0,
    )
}

fn top() -> f32 {
    area().y + koru_draw::theme::CAPTION_H
}

struct Fixture {
    roster: BotsRoster,
    commands: Receiver<Command>,
    answers: SyncSender<Answer>,
    home: WorkspaceId,
}

/// A roster over a link the test answers, about one workspace, not shown.
fn fixture() -> Fixture {
    let (link, commands, answers) = BotsLink::test_pair();
    let mut roster = BotsRoster::with_link(link);
    let home = WorkspaceId::new();
    roster.set_workspace(Some(home));
    Fixture {
        roster,
        commands,
        answers,
        home,
    }
}

impl Fixture {
    /// The one request the roster just sent: its sequence number.
    fn asked(&self) -> u64 {
        match self.commands.try_recv().expect("a request was sent") {
            Command::List { seq, workspace } => {
                assert_eq!(workspace, self.home, "about the window's workspace");
                seq
            }
            Command::Read { .. } => panic!("a roster only lists"),
        }
    }

    fn nothing_asked(&self) {
        assert!(self.commands.try_recv().is_err(), "no request was sent");
    }

    fn answer(&mut self, seq: u64, result: Result<Vec<Bot>, Failure>) -> bool {
        self.answers
            .send(Answer::Listed {
                seq,
                workspace: self.home,
                result,
            })
            .expect("the roster's end is open");
        self.roster.maintain()
    }

    /// Show the roster and answer its first request with `bots`.
    fn shown_with(&mut self, bots: Vec<Bot>) {
        self.roster.set_shown(true);
        assert!(self.roster.maintain());
        let seq = self.asked();
        assert!(self.answer(seq, Ok(bots)));
        assert_eq!(self.roster.load(), Load::Ready);
    }

    fn bot(&self, name: &str) -> Bot {
        bot(BotId::mint(), self.home, name, false)
    }

    fn ids(&self) -> Vec<BotId> {
        self.roster.bots().iter().map(|bot| bot.id).collect()
    }
}

/// The roster's frame, laid out as input and accessibility lay it out.
fn frame_of(roster: &BotsRoster) -> Frame {
    crate::input_engine::with(|engine| roster.layout(engine, area(), top()).1)
}

fn names(nodes: &[SemNode]) -> Vec<String> {
    nodes
        .iter()
        .map(|node| match &node.name {
            Some(Name::Text(text)) => text.clone(),
            Some(Name::Key(key)) => t(Lang::En, *key).to_owned(),
            None => String::new(),
        })
        .collect()
}

/// **A hidden roster does no work**: no request while it is not shown, and
/// no layout — nothing drew it and nothing asked where its rows are. Shown,
/// it asks exactly once, and asking again needs a reason.
#[test]
fn a_hidden_roster_asks_nothing_and_lays_nothing_out() {
    let mut f = fixture();
    for _ in 0..3 {
        assert!(!f.roster.maintain());
    }
    f.nothing_asked();
    assert_eq!(f.roster.work(), Work::default(), "no request, no layout");
    assert!(!f.roster.awaiting(), "a hidden roster keeps no wake clock");

    f.roster.set_shown(true);
    assert!(f.roster.maintain());
    let seq = f.asked();
    assert_eq!(f.roster.work().commands, 1);
    assert_eq!(f.roster.work().layouts, 0, "asking lays nothing out");
    assert!(f.roster.awaiting());
    assert_eq!(f.roster.load(), Load::Loading);
    assert!(!f.roster.maintain(), "a pass with nothing back is nothing");
    f.nothing_asked();

    // Its answer, after it was hidden: folded only once it is shown again,
    // and being shown again asks once more, because a hidden roster cannot
    // know what changed.
    f.roster.set_shown(false);
    f.answers
        .send(Answer::Listed {
            seq,
            workspace: f.home,
            result: Ok(vec![f.bot("Scout")]),
        })
        .unwrap();
    assert!(!f.roster.maintain());
    assert!(f.roster.bots().is_empty(), "nothing folded while hidden");
    f.nothing_asked();
    f.roster.set_shown(true);
    assert!(f.roster.maintain());
    let again = f.asked();
    assert!(again > seq);
    assert!(!f.roster.maintain());
    f.nothing_asked();
    assert_eq!(f.roster.work().commands, 2);
    assert_eq!(f.roster.work().layouts, 0);
}

/// **Only the latest request's answer is taken**: an older reply, or one
/// about another workspace, never replaces what the newer one says.
#[test]
fn a_stale_answer_is_ignored() {
    let mut f = fixture();
    f.roster.set_shown(true);
    f.roster.maintain();
    let first = f.asked();
    f.roster.set_shown(false);
    f.roster.set_shown(true);
    f.roster.maintain();
    let second = f.asked();
    assert_ne!(first, second);

    let fresh = f.bot("Fresh");
    assert!(!f.answer(first, Ok(vec![f.bot("Old")])), "the older reply");
    assert_eq!(f.roster.load(), Load::Loading);
    assert!(f.roster.bots().is_empty());
    f.answers
        .send(Answer::Listed {
            seq: second,
            workspace: WorkspaceId::new(),
            result: Ok(vec![f.bot("Elsewhere")]),
        })
        .unwrap();
    assert!(!f.roster.maintain(), "another workspace's reply");
    assert!(f.answer(second, Ok(vec![fresh.clone()])));
    assert_eq!(f.ids(), vec![fresh.id]);
    assert!(!f.roster.awaiting());
}

/// **A failed read is never "no Bots"**: what was shown stays, under a line
/// saying the service is not answering — and a roster that never had an
/// answer says that, not the empty sentence.
#[test]
fn unavailable_keeps_the_last_snapshot_and_never_reads_as_empty() {
    let mut f = fixture();
    f.roster.set_shown(true);
    f.roster.maintain();
    let seq = f.asked();
    f.answer(seq, Err(Failure::Unreachable));
    assert_eq!(f.roster.load(), Load::Unavailable);
    let frame = frame_of(&f.roster);
    assert!(frame.contains(NOTICE), "it says why");
    assert!(!frame.contains(EMPTY), "a failure is not an empty roster");

    let (a, b) = (f.bot("Scout"), f.bot("Ledger"));
    f.roster.set_shown(false);
    f.roster.set_shown(true);
    f.roster.maintain();
    let seq = f.asked();
    f.answer(seq, Ok(vec![a.clone(), b.clone()]));
    f.roster.set_shown(false);
    f.roster.set_shown(true);
    f.roster.maintain();
    let seq = f.asked();
    f.answer(seq, Err(Failure::Refused("workspace_unavailable".into())));
    assert_eq!(f.roster.load(), Load::Unavailable);
    assert_eq!(f.ids(), vec![a.id, b.id], "what was shown stays");
    let frame = frame_of(&f.roster);
    assert!(frame.contains(NOTICE));
    assert!(frame.contains(row_key(a.id)) && frame.contains(row_key(b.id)));
    assert!(!frame.contains(EMPTY));
}

/// The empty roster is the approved sentence, and **nothing to press**:
/// creating a Bot is not built yet, so no control is drawn for it.
#[test]
fn an_empty_roster_says_what_bots_are_for_and_offers_no_control() {
    let mut f = fixture();
    f.shown_with(Vec::new());
    let frame = frame_of(&f.roster);
    assert!(frame.contains(EMPTY));
    assert!(!frame.contains(NOTICE));
    assert!(frame.hits().is_empty(), "no target at all");
    assert!(
        frame
            .semantics()
            .iter()
            .all(|node| node.role != Role::Button),
        "no button"
    );
    let words = frame
        .named("Bots sentence")
        .next()
        .expect("the sentence is drawn")
        .rect;
    assert!(words.w > 0.0 && words.h > 0.0);
    assert_eq!(
        t(Lang::En, Words::BotsEmpty),
        "Give a bot a standing job. Its conversations and work stay together here."
    );
}

/// **Selection is the Bot, not the row**: a refresh that reorders and adds
/// rows keeps the card and the keyboard on the same Bots; a Bot the answer
/// no longer lists lets go of the card.
#[test]
fn selection_and_focus_survive_a_reordering_refresh() {
    let mut f = fixture();
    let (a, b, c) = (f.bot("A"), f.bot("B"), f.bot("C"));
    f.shown_with(vec![a.clone(), b.clone()]);
    assert!(f.roster.ask_open(b.id));
    assert_eq!(f.roster.take_wants_open().map(|bot| bot.id), Some(b.id));
    assert!(f.roster.set_focus(a.id));

    f.roster.set_shown(false);
    f.roster.set_shown(true);
    // Hiding lets go of the keyboard; the card stays.
    assert!(!f.roster.has_keyboard());
    assert!(f.roster.set_focus(a.id));
    f.roster.maintain();
    let seq = f.asked();
    f.answer(seq, Ok(vec![c.clone(), b.clone(), a.clone()]));
    assert_eq!(f.ids(), vec![c.id, b.id, a.id]);
    assert_eq!(f.roster.selected(), Some(b.id));
    assert_eq!(f.roster.focus(), Some(a.id));

    f.roster.set_shown(false);
    f.roster.set_shown(true);
    f.roster.set_focus(c.id);
    f.roster.maintain();
    let seq = f.asked();
    f.answer(seq, Ok(vec![a.clone()]));
    assert_eq!(f.roster.selected(), None, "B is gone");
    assert_eq!(
        f.roster.focus(),
        Some(a.id),
        "the keyboard stays in the list"
    );
}

/// **One list, named, with one item per Bot in the roster's order**, each
/// named by the Bot and offering what the keyboard and pointer do; a paused
/// Bot says so in its name, and the card is the selected state.
#[test]
fn accessibility_names_the_list_and_each_bot_in_order() {
    let mut f = fixture();
    let a = f.bot("Scout");
    let b = bot(BotId::mint(), f.home, "Ledger", true);
    f.shown_with(vec![a.clone(), b.clone()]);
    f.roster.ask_open(a.id);
    let frame = frame_of(&f.roster);
    let roots = frame.semantics();
    assert_eq!(roots.len(), 1, "the list is the roster's one root");
    let list = &roots[0];
    assert_eq!(list.role, Role::List);
    assert_eq!(list.key, Some(LIST));
    assert_eq!(names(&roots), vec!["Bots"]);
    assert_eq!(names(&list.children), vec!["Scout", "Ledger, paused"]);
    let keys: Vec<_> = list.children.iter().map(|node| node.key).collect();
    assert_eq!(keys, vec![Some(row_key(a.id)), Some(row_key(b.id))]);
    for item in &list.children {
        assert_eq!(item.role, Role::ListItem);
        assert!(item.focusable);
        assert_eq!(item.actions, vec![Action::Click, Action::Focus]);
    }
    assert_eq!(list.children[0].selected, Some(true));
    assert_eq!(list.children[1].selected, Some(false));
    assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
}

/// The words a row and the lines above it say, in every language — the
/// paused name keeps the Bot's own name, wherever the language puts it.
#[test]
fn every_word_the_roster_draws_is_in_all_eight_languages() {
    for lang in Lang::ALL {
        for key in [
            Words::BotsTitle,
            Words::BotsLoading,
            Words::BotsUnavailable,
            Words::BotsEmpty,
            Words::BotsPaused,
        ] {
            assert!(!t(lang, key).trim().is_empty(), "{lang:?} {key:?}");
        }
        let mut roster = BotsRoster::new();
        roster.set_lang(lang);
        let paused = RosterBot {
            name: "Scout".into(),
            paused: true,
            ..RosterBot::from(bot(BotId::mint(), WorkspaceId::new(), "fixture", true))
        };
        let name = roster.accessible_name(&paused);
        assert!(
            name.contains("Scout") && name != "Scout",
            "{lang:?}: {name}"
        );
    }
}

/// **Focus enters on open** on the Bot last asked for, and the arrows,
/// Home and End walk the rows by Bot; Enter asks for the Bot under the
/// keyboard; Escape gives the keyboard back. A key the roster does not
/// claim falls through.
#[test]
fn the_keyboard_walks_the_rows_and_asks_for_a_bot() {
    let mut f = fixture();
    let (a, b, c) = (f.bot("A"), f.bot("B"), f.bot("C"));
    f.shown_with(vec![a.clone(), b.clone(), c.clone()]);
    assert!(!f.roster.key(Press::Down), "nothing without the keyboard");
    f.roster.ask_open(b.id);
    f.roster.take_wants_open();
    assert!(f.roster.focus_first());
    assert_eq!(f.roster.focus(), Some(b.id), "on the Bot last asked for");
    assert!(f.roster.key(Press::Down));
    assert_eq!(f.roster.focus(), Some(c.id));
    assert!(f.roster.key(Press::Down));
    assert_eq!(f.roster.focus(), Some(c.id), "the last row stays the last");
    assert!(f.roster.key(Press::Home));
    assert_eq!(f.roster.focus(), Some(a.id));
    assert!(f.roster.key(Press::End));
    assert_eq!(f.roster.focus(), Some(c.id));
    assert!(f.roster.key(Press::Up));
    assert!(f.roster.key(Press::Open));
    assert_eq!(f.roster.take_wants_open().map(|bot| bot.id), Some(b.id));
    assert_eq!(f.roster.selected(), Some(b.id));
    assert!(f.roster.key(Press::Escape));
    assert!(!f.roster.has_keyboard());

    use winit::keyboard::{Key as WKey, NamedKey};
    assert_eq!(Press::of(&WKey::Named(NamedKey::Enter)), Some(Press::Open));
    assert_eq!(
        Press::of(&WKey::Named(NamedKey::Delete)),
        None,
        "no Delete yet"
    );
    assert_eq!(Press::of(&WKey::Character("a".into())), None);
}

/// A press on a row asks for that Bot, through the frame's own targets; a
/// press on the band beneath the rows is the roster's and asks for nothing.
#[test]
fn a_press_on_a_row_asks_for_its_bot() {
    let mut f = fixture();
    let (a, b) = (f.bot("A"), f.bot("B"));
    f.shown_with(vec![a.clone(), b.clone()]);
    let frame = frame_of(&f.roster);
    let row = frame.node(row_key(b.id)).expect("B's row").rect;
    let (x, y) = row.centre();
    assert!(f.roster.mouse_move(area(), top(), x, y));
    assert!(f.roster.mouse_down(area(), top(), x, y));
    assert_eq!(f.roster.take_wants_open().map(|bot| bot.id), Some(b.id));
    assert_eq!(f.roster.selected(), Some(b.id));
    let below = area().y + area().h - 1.0;
    assert!(
        f.roster.mouse_down(area(), top(), x, below),
        "still the roster's"
    );
    assert_eq!(f.roster.take_wants_open().map(|bot| bot.id), None);
}

/// **Another workspace is another roster**: its rows and card go at once,
/// and the new one is asked about only when shown.
#[test]
fn a_workspace_change_clears_the_rows_and_asks_again_when_shown() {
    let mut f = fixture();
    let a = f.bot("A");
    f.shown_with(vec![a.clone()]);
    f.roster.ask_open(a.id);
    let other = WorkspaceId::new();
    f.roster.set_shown(false);
    assert!(f.roster.set_workspace(Some(other)));
    assert!(!f.roster.set_workspace(Some(other)), "no change, no work");
    assert!(f.roster.bots().is_empty());
    assert_eq!(f.roster.selected(), None);
    assert_eq!(f.roster.load(), Load::Cold);
    f.roster.maintain();
    f.nothing_asked();
    f.roster.set_shown(true);
    f.roster.maintain();
    match f.commands.try_recv().expect("asked once shown") {
        Command::List { workspace, .. } => assert_eq!(workspace, other),
        Command::Read { .. } => panic!("a roster only lists"),
    }
}

/// With no link — which is every roster under test that was not given one
/// — nothing is asked and nothing claims to be loading.
#[test]
fn a_roster_with_no_link_asks_nothing() {
    let mut roster = BotsRoster::new();
    roster.start(None);
    roster.set_workspace(Some(WorkspaceId::new()));
    roster.set_shown(true);
    assert!(!roster.maintain());
    assert_eq!(roster.load(), Load::Cold);
    assert_eq!(roster.work().commands, 0);
}
