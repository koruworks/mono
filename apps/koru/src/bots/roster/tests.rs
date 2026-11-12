// SPDX-License-Identifier: MPL-2.0

use std::sync::mpsc::{Receiver, SyncSender};

use super::*;
use crate::bots::link::Listing;
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
    /// Its activity reads (S10) are skipped: see `activity_asked`.
    fn asked(&self) -> u64 {
        loop {
            match self.commands.try_recv().expect("a request was sent") {
                Command::List { seq, workspace } => {
                    assert_eq!(workspace, self.home, "about the window's workspace");
                    return seq;
                }
                Command::Activity { .. } => continue,
                _ => panic!("a roster only lists"),
            }
        }
    }

    /// The activity read the roster just sent (S10): its sequence number.
    fn activity_asked(&self) -> u64 {
        loop {
            match self.commands.try_recv().expect("an activity read was sent") {
                Command::Activity { seq, workspace } => {
                    assert_eq!(workspace, self.home, "about the window's workspace");
                    return seq;
                }
                Command::List { .. } => continue,
                _ => panic!("a roster only lists and reads activity"),
            }
        }
    }

    fn nothing_asked(&self) {
        assert!(self.commands.try_recv().is_err(), "no request was sent");
    }

    /// Answer a listing as the link's thread does: the Bots, then — the
    /// same request's second answer (S8) — its unfinished creations, none.
    fn answer(&mut self, seq: u64, result: Result<Vec<Bot>, Failure>) -> bool {
        let creations = match &result {
            Ok(_) => Ok(Listing::all(Vec::new())),
            Err(failure) => Err(failure.clone()),
        };
        self.answers
            .send(Answer::Listed {
                seq,
                workspace: self.home,
                result,
            })
            .expect("the roster's end is open");
        self.answers
            .send(Answer::Creations {
                seq,
                workspace: self.home,
                result: creations,
            })
            .expect("the roster's end is open");
        let changed = self.roster.maintain();
        let _ = frame_of(&self.roster);
        changed
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
    // Wherever a test looks at the roster, its published tree is one
    // AccessKit can take (`crate::bots::tests::assert_publishable`).
    crate::bots::tests::assert_publishable(
        "the roster",
        &crate::panes::explore::accessibility::roster_tree(roster),
    );
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

/// **A listing Koru AI did not answer is asked again while the roster is
/// shown**, on the bounded clock — not only when it is hidden and shown
/// again (the first window run: after a restart the roster said *Koru AI
/// isn't answering…* for good). A lost connection is answered as the real
/// link answers it (`Failure::lost`, `link.rs`'s `Failure::of`); a refusal
/// that would only repeat starts no clock, and a hidden roster keeps none.
#[test]
fn an_unanswered_listing_is_asked_again_while_shown() {
    let mut f = fixture();
    f.roster.set_shown(true);
    f.roster.maintain();
    let seq = f.asked();
    assert!(f.answer(seq, Err(Failure::lost())));
    assert_eq!(f.roster.load(), Load::Unavailable);
    assert!(f.roster.awaiting(), "a clock runs while shown");
    assert!(f.roster.list_retry_due_for_test());
    assert!(f.roster.maintain());
    let again = f.asked();
    assert!(again > seq, "asked again without being shown again");
    assert!(f.answer(again, Ok(vec![f.bot("Scout")])));
    assert_eq!(f.roster.load(), Load::Ready);
    assert!(!f.roster.list_retry_due_for_test(), "answered: no clock");

    // A refusal the owner says is final: no clock.
    f.roster.owe_lifecycle_refresh();
    f.roster.maintain();
    let refused = f.asked();
    f.answer(refused, Err(Failure::Refused("wrong_owner".into())));
    assert!(!f.roster.list_retry_due_for_test(), "nothing to ask again");

    // Hidden, the clock stops; shown again, it asks once as it always did.
    f.roster.owe_lifecycle_refresh();
    f.roster.maintain();
    let lost = f.asked();
    f.answer(lost, Err(Failure::lost()));
    f.roster.set_shown(false);
    assert!(!f.roster.list_retry_due_for_test(), "no clock while hidden");
}

/// **A listing refused before the window is vouched for is asked again,
/// said as loading, and after a bounded while said as no access** — never
/// *Koru AI isn't answering…*, because Koru AI did answer (the second window
/// run, 2 of 2 restarts: `failure="forbidden"` 3 ms after the first frame,
/// and the roster said *isn't answering* until it was hidden and shown
/// again). The refusal is answered as the real link answers it: the AI
/// worker's `authorize` refusing a caller with no `bots.manage`, classified
/// by `link.rs`'s `Failure::of` (`Failure::unvouched_refusal`).
#[test]
fn a_listing_refused_before_the_window_is_vouched_for_is_asked_again() {
    let unvouched = Failure::unvouched_refusal;
    let published = |roster: &BotsRoster| {
        crate::panes::explore::accessibility::roster_tree(roster)
            .iter()
            .filter_map(|node| node.label.clone())
            .collect::<Vec<_>>()
    };
    let mut f = fixture();
    f.roster.set_shown(true);
    f.roster.maintain();
    let seq = f.asked();
    f.answer(seq, Err(unvouched()));
    assert_ne!(
        f.roster.notice(),
        Some(Words::BotsUnavailable),
        "Koru AI answered: it is not *isn't answering*"
    );
    assert_eq!(f.roster.notice(), Some(Words::BotsLoading));
    assert!(f.roster.awaiting(), "a clock runs while shown");
    assert!(f.roster.list_retry_due_for_test(), "asked again");
    assert!(f.roster.maintain());
    let again = f.asked();
    assert!(again > seq, "without being hidden and shown again");
    f.answer(again, Ok(vec![f.bot("Scout")]));
    assert_eq!(f.roster.load(), Load::Ready, "vouched for: listed");

    // Refused for longer than a starting Koru takes: no more asking, the
    // rows shown stay, and the sentence is about access.
    f.roster.owe_lifecycle_refresh();
    f.roster.maintain();
    let seq = f.asked();
    f.answer(seq, Err(unvouched()));
    assert_eq!(f.roster.load(), Load::Ready, "rows shown stay as they were");
    f.roster
        .vouching
        .age_for_test(crate::bots::link::UNVOUCHED_LIMIT);
    assert!(f.roster.list_retry_due_for_test());
    f.roster.maintain();
    let seq = f.asked();
    f.answer(seq, Err(unvouched()));
    assert_eq!(f.roster.load(), Load::Denied);
    assert_eq!(f.roster.notice(), Some(Words::BotsNoAccess));
    assert!(!f.roster.list_retry_due_for_test(), "no more asking");
    assert_eq!(f.roster.bots().len(), 1, "what was shown stays");
    assert!(
        published(&f.roster).contains(&t(Lang::En, Words::BotsNoAccess).to_owned()),
        "published as drawn: {:?}",
        published(&f.roster)
    );

    // Shown again, it has the whole while again.
    f.roster.set_shown(false);
    f.roster.set_shown(true);
    f.roster.maintain();
    let seq = f.asked();
    f.answer(seq, Err(unvouched()));
    assert_eq!(f.roster.notice(), Some(Words::BotsLoading));
    assert!(f.roster.list_retry_due_for_test());
}

/// **A Bot tab's own news reaches its row at once** — renamed, paused —
/// and a Bot the roster does not list is left to the list.
#[test]
fn a_tabs_news_changes_its_row_at_once() {
    let mut f = fixture();
    let scout = f.bot("Scout");
    f.shown_with(vec![scout.clone()]);
    let mut renamed = scout.clone();
    renamed.config.name = "Scout Renamed".into();
    renamed.paused = true;
    assert!(f.roster.know_bot(RosterBot::from(renamed)));
    assert_eq!(f.roster.bots()[0].name, "Scout Renamed");
    assert!(f.roster.bots()[0].paused);
    assert!(!f.roster.know_bot(RosterBot::from(f.bot("Other"))));
    assert_eq!(f.roster.bots().len(), 1);
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
    // The listing is followed by its activity read (S10); answered, nothing
    // more is owed.
    let activity = f.activity_asked();
    f.answers
        .send(Answer::Activity {
            seq: activity,
            workspace: f.home,
            result: Ok(activity_page(Vec::new(), false)),
        })
        .unwrap();
    f.roster.maintain();
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
    f.answer(seq, Err(Failure::lost()));
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

/// The empty roster is the approved sentence, and **two things to press**:
/// *Create a bot* (S8), which the window opens as a create tab, and the
/// Archived filter (S14).
#[test]
fn an_empty_roster_says_what_bots_are_for_and_offers_create() {
    let mut f = fixture();
    f.shown_with(Vec::new());
    let frame = frame_of(&f.roster);
    assert!(frame.contains(EMPTY));
    assert!(!frame.contains(NOTICE));
    let targets: Vec<_> = frame.hits().into_iter().map(|hit| hit.key).collect();
    assert_eq!(
        targets,
        [CREATE, ARCHIVED_TOGGLE],
        "Create, then the filter"
    );
    let _ = f.commands.try_iter().count();
    assert!(f.roster.press_unfinished(CREATE));
    assert!(f.roster.take_wants_create(), "the window opens the form");
    assert!(!f.roster.take_wants_create(), "once");
    f.nothing_asked();
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
    assert_eq!(f.roster.focus(), Some(row_key(a.id)));

    f.roster.set_shown(false);
    f.roster.set_shown(true);
    f.roster.set_focus(c.id);
    f.roster.maintain();
    let seq = f.asked();
    f.answer(seq, Ok(vec![a.clone()]));
    assert_eq!(f.roster.selected(), None, "B is gone");
    assert_eq!(
        f.roster.focus(),
        Some(row_key(a.id)),
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
    // The Bots, then the Archived filter's toggle (S14).
    assert_eq!(
        names(&list.children),
        vec!["Scout", "Ledger, paused", "Show archived bots"]
    );
    let keys: Vec<_> = list.children.iter().map(|node| node.key).collect();
    assert_eq!(
        keys,
        vec![
            Some(row_key(a.id)),
            Some(row_key(b.id)),
            Some(ARCHIVED_TOGGLE)
        ]
    );
    assert_eq!(list.children[2].role, Role::Button);
    for item in &list.children[..2] {
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
    assert_eq!(
        f.roster.focus(),
        Some(row_key(b.id)),
        "on the Bot last asked for"
    );
    assert!(f.roster.key(Press::Down));
    assert_eq!(f.roster.focus(), Some(row_key(c.id)));
    // Past the last Bot: the Archived filter's toggle, the last control.
    assert!(f.roster.key(Press::Down));
    assert_eq!(f.roster.focus(), Some(ARCHIVED_TOGGLE));
    assert!(f.roster.key(Press::Down));
    assert_eq!(
        f.roster.focus(),
        Some(ARCHIVED_TOGGLE),
        "the last control stays the last"
    );
    assert!(f.roster.key(Press::Home));
    assert_eq!(f.roster.focus(), Some(row_key(a.id)));
    assert!(f.roster.key(Press::End));
    assert_eq!(f.roster.focus(), Some(ARCHIVED_TOGGLE));
    assert!(f.roster.key(Press::Up));
    assert!(f.roster.key(Press::Up));
    assert_eq!(f.roster.focus(), Some(row_key(b.id)));
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
    f.activity_asked();
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
        _ => panic!("a roster only lists"),
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

// ------------------------------------------ running and Needs-you (S10)

fn activity_page(
    entries: Vec<(BotId, koru_ai_core::ChatId, BotActivityState)>,
    truncated: bool,
) -> BotActivityPage {
    BotActivityPage {
        entries: entries
            .into_iter()
            .map(|(bot_id, chat_id, state)| BotActivity {
                bot_id,
                chat_id,
                state,
            })
            .collect(),
        truncated,
    }
}

/// **The roster reads its activity after its own listing, once, and only
/// while shown** — coalesced: one read on its way at a time, and the one
/// owed meanwhile goes after it. A hidden roster asks nothing.
#[test]
fn activity_is_read_after_the_listing_once_and_coalesced() {
    let mut f = fixture();
    let scout = f.bot("Scout");
    f.shown_with(vec![scout.clone()]);
    let seq = f.activity_asked();
    f.nothing_asked();
    // Owed twice while one is on its way: one more, after it.
    f.roster.owe_activity();
    f.roster.owe_activity();
    assert!(!f.roster.maintain());
    f.nothing_asked();
    assert!(f.roster.awaiting(), "an answer is owed");
    f.answers
        .send(Answer::Activity {
            seq,
            workspace: f.home,
            result: Ok(activity_page(Vec::new(), false)),
        })
        .unwrap();
    assert!(f.roster.maintain());
    f.activity_asked();
    f.nothing_asked();
    // Hidden: owed, but nothing goes.
    f.roster.set_shown(false);
    f.roster.owe_activity();
    assert!(!f.roster.maintain());
    f.nothing_asked();
}

/// **Each running or Needs-you conversation sits under its Bot**, wearing
/// the Chats list's mark and saying its state in words — the main one by
/// name — and a press opens that conversation. A Bot with none says
/// nothing: never *idle*. A read that stopped at its bound says more work
/// exists; a failed read keeps what was shown.
#[test]
fn running_and_needs_you_conversations_sit_under_their_bot() {
    let mut f = fixture();
    let (scout, quiet) = (f.bot("Scout"), f.bot("Quiet"));
    f.shown_with(vec![scout.clone(), quiet.clone()]);
    let seq = f.activity_asked();
    let other = koru_ai_core::ChatId::new();
    f.answers
        .send(Answer::Activity {
            seq,
            workspace: f.home,
            result: Ok(activity_page(
                vec![
                    (scout.id, scout.main_chat, BotActivityState::NeedsYou),
                    (scout.id, other, BotActivityState::Running),
                ],
                true,
            )),
        })
        .unwrap();
    assert!(f.roster.maintain());
    let frame = frame_of(&f.roster);
    assert!(frame.contains(activity_key(scout.main_chat)));
    assert!(frame.contains(activity_key(other)));
    assert!(frame.contains(MORE_WORK), "more work than is listed: said");
    let listed = f.roster.bots()[0].clone();
    let names: Vec<String> = f
        .roster
        .activity()
        .iter()
        .map(|entry| f.roster.activity_name(&listed, entry))
        .collect();
    assert_eq!(
        names,
        [
            format!(
                "{}, {}",
                t(Lang::En, Words::BotsMainConversation),
                t(Lang::En, Words::ChatsStatusNeedsYou)
            ),
            format!(
                "{}, {}",
                t(Lang::En, Words::BotsOtherConversation),
                t(Lang::En, Words::ChatsStatusWorking)
            ),
        ]
    );
    // **Once an open tab has listed it, the row says the conversation's own
    // title** — the words its row in the tab shows (the second window run:
    // the roster said *Another conversation* for a conversation the list
    // titled otherwise).
    assert!(f.roster.know_titles([(other, "Builder forums".to_owned())]));
    assert_eq!(
        f.roster.activity_name(&listed, &f.roster.activity()[1]),
        format!("Builder forums, {}", t(Lang::En, Words::ChatsStatusWorking))
    );
    assert!(!f.roster.know_titles([(other, "Builder forums".to_owned())]));
    let statuses = f.roster.activity_of(scout.id);
    assert_eq!(statuses.get(&other), Some(&ChatStatus::Working));
    assert!(f.roster.activity_of(quiet.id).is_empty(), "nothing claimed");

    // A press on the Running sub-row asks for that conversation.
    let rect = frame.node(activity_key(other)).expect("its row").rect;
    assert!(
        f.roster
            .mouse_down(area(), top(), rect.x + rect.w * 0.5, rect.y + rect.h * 0.5)
    );
    let (bot, chat) = f.roster.take_wants_open_chat().expect("asked");
    assert_eq!((bot.id, chat), (scout.id, other));
    assert!(f.roster.take_wants_open().is_none(), "not the Bot row");

    // A failed read keeps what was shown.
    f.roster.owe_activity();
    f.roster.maintain();
    let again = f.activity_asked();
    f.answers
        .send(Answer::Activity {
            seq: again,
            workspace: f.home,
            result: Err(Failure::lost()),
        })
        .unwrap();
    f.roster.maintain();
    assert_eq!(f.roster.activity().len(), 2);
}

/// Another workspace's activity is never this roster's.
#[test]
fn another_workspaces_activity_is_ignored() {
    let mut f = fixture();
    let scout = f.bot("Scout");
    f.shown_with(vec![scout.clone()]);
    let seq = f.activity_asked();
    assert!(!f.roster.take_activity(
        seq,
        WorkspaceId::new(),
        Ok(activity_page(
            vec![(scout.id, scout.main_chat, BotActivityState::Running)],
            false
        ))
    ));
    assert!(f.roster.activity().is_empty());
}

/// The Archived filter's request, skipping the roster's other reads.
fn archived_asked(f: &Fixture) -> u64 {
    loop {
        match f.commands.try_recv().expect("the Archived filter is asked") {
            Command::Lifecycle {
                seq,
                workspace,
                lifecycle,
            } => {
                assert_eq!(lifecycle, BotLifecycle::Archived);
                assert_eq!(workspace, f.home);
                return seq;
            }
            _ => continue,
        }
    }
}

/// **The Archived filter** (S14): closed, it asks nothing; opened, it asks
/// the owner's archived list once and lists each Bot, which opens its tab;
/// *Unarchive* is offered only where the window knows the Archive, and it
/// sends the Restore naming it at the listed revision. A hidden roster asks
/// nothing for it.
#[test]
fn the_archived_filter_lists_opens_and_unarchives() {
    let mut f = fixture();
    f.shown_with(Vec::new());
    f.roster.maintain();
    assert!(
        f.commands
            .try_iter()
            .all(|command| !matches!(command, Command::Lifecycle { .. })),
        "closed: nothing asked"
    );
    assert!(f.roster.press_archived(ARCHIVED_TOGGLE));
    assert_eq!(f.roster.archived_notice(), Some(Words::BotsArchivedLoading));
    f.roster.maintain();
    let seq = archived_asked(&f);
    let mut known = f.bot("Scout");
    known.lifecycle = BotLifecycle::Archived;
    known.revision = 4;
    // The owner names the Archive that holds it (`held_by`, filled in by
    // `with_lifecycle_holders` for `list_lifecycle`); this roster is fresh,
    // as after a restart, and remembers nothing of its own.
    let archive = BotRequestId::mint();
    known.held_by = Some(koru_ai_core::bots::BotLifecycleHolder {
        request_id: archive,
        kind: BotLifecycleKind::Archive,
        completed_at_ms: 7,
    });
    let mut older = f.bot("Older");
    older.lifecycle = BotLifecycle::Archived;
    f.answers
        .send(Answer::Lifecycle {
            seq,
            workspace: f.home,
            lifecycle: BotLifecycle::Archived,
            result: Ok(vec![known.clone(), older.clone()]),
        })
        .unwrap();
    assert!(f.roster.maintain());
    let frame = frame_of(&f.roster);
    assert!(frame.contains(archived_key(known.id)) && frame.contains(archived_key(older.id)));
    assert!(frame.contains(unarchive_key(known.id)));
    assert!(
        !frame.contains(unarchive_key(older.id)),
        "no Unarchive where the owner names no Archive"
    );
    assert_eq!(f.roster.archived_notice(), Some(Words::BotsUndoUnavailable));
    // A row opens its Bot's tab.
    assert!(f.roster.press_archived(archived_key(older.id)));
    assert_eq!(f.roster.take_wants_open().map(|row| row.id), Some(older.id));
    // Unarchive sends the Restore naming the Archive.
    assert!(f.roster.press_archived(unarchive_key(known.id)));
    let (request, command) = loop {
        match f.commands.try_recv().expect("a write") {
            Command::Write {
                request, command, ..
            } => break (request, *command),
            _ => continue,
        }
    };
    assert_eq!(
        command,
        BotCommand::Restore {
            request_id: request,
            bot_id: known.id,
            expected_revision: 4,
            source_request_id: archive
        }
    );
    assert!(f.roster.unarchiving(known.id));
    f.answers
        .send(Answer::Wrote {
            bot: known.id,
            request,
            result: Ok(BotResponse::Lifecycle {
                operation: koru_ai_core::bots::BotLifecycleProgress {
                    request_id: request,
                    bot_id: known.id,
                    kind: BotLifecycleKind::Restore,
                    chat_id: None,
                    source_request_id: Some(archive),
                    stage: BotLifecycleStage::Completed,
                    refusal: None,
                    members: Vec::new(),
                    left_in_place: Vec::new(),
                },
            }),
        })
        .unwrap();
    f.roster.maintain();
    let _ = frame_of(&f.roster);
    let finished = f.roster.take_finished();
    assert_eq!(finished.len(), 1);
    assert_eq!(finished[0].reverses, Some(BotLifecycleKind::Archive));
    // Both lists are read again; hidden, nothing is.
    f.roster.set_shown(false);
    f.roster.maintain();
    let _ = f.commands.try_iter().count();
    f.roster.maintain();
    f.nothing_asked();
}

// ------------------------------------------------- unfinished creations (S8)

/// An unfinished creation of `home`, as the owner lists it.
fn unfinished(home: WorkspaceId, name: &str, retryable: bool) -> BotCreation {
    BotCreation {
        request_id: BotRequestId::mint(),
        bot_id: BotId::mint(),
        home_workspace: home,
        name: name.to_owned(),
        stage: BotCreationStage::Incomplete,
        phase: koru_ai_core::bots::BotCreationPhase::RolePending,
        reason: Some(if retryable {
            koru_ai_core::bots::BotCreationReason::StoreUnavailable
        } else {
            koru_ai_core::bots::BotCreationReason::RoleMissing
        }),
        retryable,
        role_page: None,
        role_page_unconfirmed: false,
        folder_binding: None,
        left_behind: None,
        bot: None,
    }
}

/// **A create tab's news reaches the Unfinished group at once** — shown or
/// not, before the roster lists again (the second window run: the group
/// appeared only once the roster was shown again). The creation is the
/// owner's own answer to the tab (`ReadBotCreation`/`CreateBotWithRole`,
/// stage `Incomplete`); one that ended leaves the group; another
/// workspace's is not this roster's.
#[test]
fn a_create_tabs_news_reaches_the_unfinished_group_at_once() {
    let mut f = fixture();
    f.shown_with(Vec::new());
    f.roster.set_shown(false);
    let stuck = unfinished(f.home, "Dark Check", true);
    assert!(f.roster.know_creation(stuck.clone()), "known at once");
    assert_eq!(f.roster.creations(), [stuck.clone()]);
    assert!(
        frame_of(&f.roster).contains(creation_retry_key(stuck.request_id)),
        "drawn with its Retry"
    );
    assert!(
        !f.roster.know_creation(stuck.clone()),
        "the same: nothing moved"
    );
    let mut done = stuck.clone();
    done.stage = BotCreationStage::Completed;
    assert!(f.roster.know_creation(done), "finished: out of the group");
    assert!(f.roster.creations().is_empty());
    let elsewhere = unfinished(WorkspaceId::new(), "Other", true);
    assert!(!f.roster.know_creation(elsewhere));
    assert!(f.roster.creations().is_empty());
}

/// **An unfinished row gives its state the row's whole width, under its
/// name and buttons, and nothing overlaps it** at the navigator's narrowest
/// width (the second window run, `scr-b12-04`: *The working folder
/// couldn't be made or read.* wrapped into a column about 90 px wide and
/// ran over *Show archived bots*). Laid out at the narrowest column and at
/// a wide one: the wrapped state is taller where it wraps — so its measured
/// height follows its lines — and the toggle starts below it.
#[test]
fn an_unfinished_rows_state_has_the_rows_width_and_overlaps_nothing() {
    let mut f = fixture();
    f.shown_with(Vec::new());
    let mut stuck = unfinished(f.home, "Dark Check", true);
    stuck.reason = Some(koru_ai_core::bots::BotCreationReason::FolderUnavailable);
    assert!(f.roster.know_creation(stuck.clone()));
    let narrow = Rect::new(0.0, area().y, koru_draw::theme::SIDEBAR_MIN_W, 600.0);
    let wide = Rect::new(0.0, area().y, 1200.0, 600.0);
    let rects = |within: Rect| {
        crate::input_engine::with(|engine| {
            let (_, frame) = f.roster.layout(engine, within, top());
            let rect = |key| frame.node(key).map(|node| node.rect).expect("drawn");
            (
                rect(creation_state_key(stuck.request_id)),
                rect(creation_retry_key(stuck.request_id)),
                rect(creation_abandon_key(stuck.request_id)),
                rect(ARCHIVED_TOGGLE),
            )
        })
    };
    let (state, retry, abandon, toggle) = rects(narrow);
    assert!(
        state.w >= narrow.w * 0.75,
        "the state has the row's width: {state:?} in {narrow:?}"
    );
    assert!(
        state.y >= retry.y + retry.h - 0.5 && state.y >= abandon.y + abandon.h - 0.5,
        "under the name and its buttons: {state:?} {retry:?} {abandon:?}"
    );
    assert!(
        toggle.y >= state.y + state.h - 0.5,
        "Show archived bots starts below it: {toggle:?} {state:?}"
    );
    let (wide_state, ..) = rects(wide);
    assert!(
        state.h > wide_state.h + 1.0,
        "wrapped, it is measured taller: {} at {} px, {} at {} px",
        state.h,
        narrow.w,
        wide_state.h,
        wide.w
    );
}

/// **A Retry or an Abandon from the group tells the window which creation
/// moved**, whatever it came to, so a create tab holding it reads it again
/// (the second window run: the tab went on offering Retry and Abandon).
#[test]
fn a_retry_from_the_group_says_which_creation_moved() {
    let mut f = fixture();
    f.shown_with(Vec::new());
    let stuck = unfinished(f.home, "Dark Check", true);
    f.roster.know_creation(stuck.clone());
    let _ = f.commands.try_iter().count();
    assert!(f.roster.retry(stuck.request_id));
    let _ = f.commands.try_iter().count();
    assert!(
        f.roster.take_creations_moved().is_empty(),
        "not before it answers"
    );
    let mut done = stuck.clone();
    done.stage = BotCreationStage::Completed;
    f.answers
        .send(Answer::Wrote {
            bot: stuck.bot_id,
            request: stuck.request_id,
            result: Ok(BotResponse::Creation { creation: done }),
        })
        .unwrap();
    f.roster.maintain();
    assert_eq!(f.roster.take_creations_moved(), [stuck.request_id]);
    assert!(f.roster.take_creations_moved().is_empty(), "taken once");
}

/// **A creation a restart interrupted is never invisible**: a roster made
/// fresh — as after a restart, knowing nothing of any draft — lists the
/// workspace's unfinished creations beside its Bots, from the listing's
/// second answer, in an *Unfinished* group with Retry where Retry can
/// finish it and Abandon, each by the creation's request alone.
#[test]
fn unfinished_creations_are_listed_after_a_restart_and_acted_on_by_id() {
    let mut f = fixture();
    f.roster.set_shown(true);
    assert!(f.roster.maintain());
    let seq = f.asked();
    f.answers
        .send(Answer::Listed {
            seq,
            workspace: f.home,
            result: Ok(Vec::new()),
        })
        .unwrap();
    assert!(f.roster.maintain());
    assert!(f.roster.awaiting(), "the creations are still owed");
    let (stuck, ended) = (
        unfinished(f.home, "Scout", true),
        unfinished(f.home, "CFO", false),
    );
    f.answers
        .send(Answer::Creations {
            seq,
            workspace: f.home,
            result: Ok(Listing::all(vec![stuck.clone(), ended.clone()])),
        })
        .unwrap();
    assert!(f.roster.maintain());
    assert_eq!(f.roster.creations().len(), 2);
    let frame = frame_of(&f.roster);
    assert!(frame.contains(UNFINISHED_HEADING));
    assert!(frame.contains(creation_retry_key(stuck.request_id)));
    assert!(frame.contains(creation_abandon_key(stuck.request_id)));
    assert!(
        !frame.contains(creation_retry_key(ended.request_id)),
        "no Retry where Retry cannot finish it"
    );
    assert!(frame.contains(creation_abandon_key(ended.request_id)));
    assert_eq!(
        f.roster.creation_accessible_name(&stuck),
        koru_i18n::fill(t(Lang::En, Words::BotsUnfinishedRow), &["Scout"])
    );
    assert_eq!(
        f.roster.creation_state(&ended),
        t(Lang::En, Words::BotsCreateRoleMissing)
    );
    let _ = f.commands.try_iter().count();

    // Retry, by the request alone.
    assert!(
        f.roster
            .press_unfinished(creation_retry_key(stuck.request_id))
    );
    match f.commands.try_recv().expect("a retry") {
        Command::Write {
            bot,
            request,
            command,
        } => {
            assert_eq!((bot, request), (stuck.bot_id, stuck.request_id));
            assert_eq!(
                *command,
                BotCommand::RetryBotCreation {
                    request_id: stuck.request_id
                }
            );
        }
        _ => panic!("a write"),
    }
    assert!(
        !f.roster.retry(stuck.request_id),
        "a second press sends nothing"
    );

    // Abandon is asked about first; confirmed, it is sent by the request.
    assert!(
        f.roster
            .press_unfinished(creation_abandon_key(ended.request_id))
    );
    f.nothing_asked();
    assert_eq!(
        f.roster.take_abandon_ask(),
        Some((ended.request_id, "CFO".to_owned()))
    );
    assert!(f.roster.abandon(ended.request_id));
    let Ok(Command::Write { command, .. }) = f.commands.try_recv() else {
        panic!("an abandon");
    };
    assert_eq!(
        *command,
        BotCommand::AbandonBotCreation {
            request_id: ended.request_id
        }
    );
    let page = koru_ai_core::bots::BotPage {
        workspace: f.home,
        document: koru_ai_core::DocumentId::new(),
    };
    let mut abandoned = ended.clone();
    abandoned.stage = BotCreationStage::Abandoned;
    abandoned.left_behind = Some(koru_ai_core::bots::BotCreationLeftovers {
        role_page: Some(page.clone()),
        folder: None,
    });
    f.answers
        .send(Answer::Wrote {
            bot: ended.bot_id,
            request: ended.request_id,
            result: Ok(BotResponse::Creation {
                creation: abandoned,
            }),
        })
        .unwrap();
    assert!(f.roster.maintain());
    assert_eq!(f.roster.creations().len(), 1, "no longer unfinished");
    assert!(f.roster.abandoned().is_some(), "what it left is said");
    let frame = frame_of(&f.roster);
    assert!(frame.contains(OPEN_LEFT_PAGE));
    assert!(f.roster.press_unfinished(OPEN_LEFT_PAGE));
    assert_eq!(f.roster.take_open_page(), Some(page));
}

/// **A failed read of the unfinished creations keeps what was listed**, and
/// an answer for an older listing or another workspace is not taken.
#[test]
fn a_stale_or_failed_creations_answer_keeps_what_was_listed() {
    let mut f = fixture();
    f.roster.set_shown(true);
    f.roster.maintain();
    let seq = f.asked();
    f.answer(seq, Ok(Vec::new()));
    let stuck = unfinished(f.home, "Scout", true);
    f.answers
        .send(Answer::Creations {
            seq,
            workspace: f.home,
            result: Ok(Listing::all(vec![stuck.clone()])),
        })
        .unwrap();
    f.roster.maintain();
    for (seq, workspace, result) in [
        (seq, f.home, Err(Failure::lost())),
        (seq.wrapping_add(7), f.home, Ok(Listing::all(Vec::new()))),
        (seq, WorkspaceId::new(), Ok(Listing::all(Vec::new()))),
    ] {
        f.answers
            .send(Answer::Creations {
                seq,
                workspace,
                result,
            })
            .unwrap();
        f.roster.maintain();
        assert_eq!(
            f.roster
                .creations()
                .iter()
                .map(|creation| creation.request_id)
                .collect::<Vec<_>>(),
            [stuck.request_id]
        );
    }
}

/// **A creation retried from the Unfinished group is followed until it
/// finishes**: while it moves and the roster is shown, the roster lists
/// again on the create form's bounded clock; once the owner no longer lists
/// it as unfinished it leaves the group. An Abandon of one that finished
/// meanwhile — `abandon_bot_creation` answers a completed creation
/// `request_conflict` — says it was created, not that the request differed.
#[test]
fn a_retried_creation_is_followed_until_it_finishes() {
    let mut f = fixture();
    f.roster.set_shown(true);
    f.roster.maintain();
    let seq = f.asked();
    let stuck = unfinished(f.home, "Scout", true);
    let mut moving = stuck.clone();
    moving.stage = BotCreationStage::Pending;
    moving.reason = None;
    moving.retryable = false;
    for (listed, seq) in [(stuck.clone(), seq)] {
        f.answers
            .send(Answer::Listed {
                seq,
                workspace: f.home,
                result: Ok(Vec::new()),
            })
            .unwrap();
        f.answers
            .send(Answer::Creations {
                seq,
                workspace: f.home,
                result: Ok(Listing::all(vec![listed])),
            })
            .unwrap();
    }
    f.roster.maintain();
    let _ = f.commands.try_iter().count();
    assert!(f.roster.retry(stuck.request_id));
    let _ = f.commands.try_iter().count();
    f.answers
        .send(Answer::Wrote {
            bot: stuck.bot_id,
            request: stuck.request_id,
            result: Ok(BotResponse::Creation {
                creation: moving.clone(),
            }),
        })
        .unwrap();
    f.roster.maintain();
    f.roster.maintain();
    let seq = f.asked();
    f.answers
        .send(Answer::Listed {
            seq,
            workspace: f.home,
            result: Ok(Vec::new()),
        })
        .unwrap();
    f.answers
        .send(Answer::Creations {
            seq,
            workspace: f.home,
            result: Ok(Listing::all(vec![moving.clone()])),
        })
        .unwrap();
    f.roster.maintain();
    assert!(f.roster.awaiting(), "a moving creation keeps the clock");
    std::thread::sleep(std::time::Duration::from_millis(300));
    f.roster.maintain();
    let seq = f.asked();
    // Finished: the owner lists the Bot, and no unfinished creation.
    let made = bot(stuck.bot_id, f.home, "Scout", false);
    f.answers
        .send(Answer::Listed {
            seq,
            workspace: f.home,
            result: Ok(vec![made.clone()]),
        })
        .unwrap();
    f.answers
        .send(Answer::Creations {
            seq,
            workspace: f.home,
            result: Ok(Listing::all(Vec::new())),
        })
        .unwrap();
    f.roster.maintain();
    assert!(f.roster.creations().is_empty(), "it left the group");
    assert_eq!(f.ids(), vec![made.id], "and its Bot is listed");
    let _ = f.commands.try_iter().count();
    f.roster.set_shown(false);
    assert!(!f.roster.awaiting(), "no clock once nothing moves");
}

/// **An Abandon or a Retry answered `request_conflict` reads the creation,
/// and its stage speaks** (the second review's item 5). The owner answers
/// it for more than a creation that was made: `abandon_creation` for one
/// that finished either way — made, or rejected before it made anything —
/// and `abandon_bot_creation` and `advance_bot_creation` when the record
/// moved between their read and their write. The roster says *Created* only
/// when the owner's read says it was made.
#[test]
fn a_creation_conflict_reads_the_creation_and_its_stage_speaks() {
    for (stage, said) in [
        (BotCreationStage::Completed, Some(Words::BotsCreateCreated)),
        (
            BotCreationStage::Rejected,
            Some(Words::BotsUnfinishedRejected),
        ),
        (BotCreationStage::Incomplete, None),
    ] {
        let mut g = fixture();
        g.roster.set_shown(true);
        g.roster.maintain();
        let seq = g.asked();
        let other = unfinished(g.home, "CFO", true);
        g.answers
            .send(Answer::Listed {
                seq,
                workspace: g.home,
                result: Ok(Vec::new()),
            })
            .unwrap();
        g.answers
            .send(Answer::Creations {
                seq,
                workspace: g.home,
                result: Ok(Listing::all(vec![other.clone()])),
            })
            .unwrap();
        g.roster.maintain();
        let _ = g.commands.try_iter().count();
        assert!(g.roster.abandon(other.request_id));
        let _ = g.commands.try_iter().count();
        g.answers
            .send(Answer::Wrote {
                bot: other.bot_id,
                request: other.request_id,
                result: Err(Failure::Refused("request_conflict".into())),
            })
            .unwrap();
        g.roster.maintain();
        assert_ne!(
            g.roster.creation_refused(),
            Some(Words::BotsCreateCreated),
            "{stage:?}: nothing claimed before the read"
        );
        let read = g
            .commands
            .try_iter()
            .find_map(|command| match command {
                Command::Write {
                    request, command, ..
                } => Some((request, *command)),
                _ => None,
            })
            .expect("the creation is read");
        assert_eq!(
            read,
            (
                other.request_id,
                BotCommand::ReadBotCreation {
                    request_id: other.request_id
                }
            )
        );
        assert!(g.roster.creation_acting(other.request_id), "{stage:?}");
        let mut now = other.clone();
        now.stage = stage;
        g.answers
            .send(Answer::Wrote {
                bot: other.bot_id,
                request: other.request_id,
                result: Ok(BotResponse::Creation { creation: now }),
            })
            .unwrap();
        g.roster.maintain();
        assert_eq!(g.roster.creation_refused(), said, "{stage:?}");
        assert_eq!(
            g.roster.creations().is_empty(),
            stage != BotCreationStage::Incomplete,
            "{stage:?}"
        );
    }
}

/// **An Unarchive that answered takes its row with it at once**, as the
/// Trash's Restore does: a late second press sends nothing the owner would
/// refuse (`require_source`).
#[test]
fn an_unarchive_that_answered_leaves_the_archived_filter_at_once() {
    let mut f = fixture();
    f.shown_with(Vec::new());
    assert!(f.roster.press_archived(ARCHIVED_TOGGLE));
    f.roster.maintain();
    let seq = archived_asked(&f);
    let mut known = f.bot("Scout");
    known.lifecycle = BotLifecycle::Archived;
    let archive = BotRequestId::mint();
    known.held_by = Some(koru_ai_core::bots::BotLifecycleHolder {
        request_id: archive,
        kind: BotLifecycleKind::Archive,
        completed_at_ms: 7,
    });
    f.answers
        .send(Answer::Lifecycle {
            seq,
            workspace: f.home,
            lifecycle: BotLifecycle::Archived,
            result: Ok(vec![known.clone()]),
        })
        .unwrap();
    f.roster.maintain();
    assert!(f.roster.unarchive(known.id));
    let request = f
        .commands
        .try_iter()
        .find_map(|command| match command {
            Command::Write { request, .. } => Some(request),
            _ => None,
        })
        .expect("a Restore");
    f.answers
        .send(Answer::Wrote {
            bot: known.id,
            request,
            result: Ok(BotResponse::Lifecycle {
                operation: koru_ai_core::bots::BotLifecycleProgress {
                    request_id: request,
                    bot_id: known.id,
                    kind: BotLifecycleKind::Restore,
                    chat_id: None,
                    source_request_id: Some(archive),
                    stage: BotLifecycleStage::Completed,
                    refusal: None,
                    members: Vec::new(),
                    left_in_place: Vec::new(),
                },
            }),
        })
        .unwrap();
    f.roster.maintain();
    assert!(f.roster.archived().is_empty(), "its row is gone at once");
    assert!(!f.roster.unarchive(known.id), "a late second press");
    assert!(
        !f.commands
            .try_iter()
            .any(|command| matches!(command, Command::Write { .. })),
        "no second Restore"
    );
}

/// **An Unarchive Koru AI did not answer, or refused because it was done
/// already**, is never said as not unarchived: the real link reads a lost
/// connection as unanswered, and `request_conflict` and `revision_conflict`
/// say it was restored already or moved; the lists read again say which.
#[test]
fn an_unarchive_whose_outcome_is_unknown_is_never_said_as_refused() {
    use crate::bots::link::{Said, wrote};
    for (result, words) in [
        (
            wrote(Err(Said::new("gateway_unavailable", true))),
            Words::BotsErrUnreachable,
        ),
        (
            Err(Failure::Refused("request_conflict".into())),
            Words::BotsErrRestoreConflict,
        ),
        (
            Err(Failure::Refused("revision_conflict".into())),
            Words::BotsErrRestoreConflict,
        ),
    ] {
        let mut f = fixture();
        f.shown_with(Vec::new());
        assert!(f.roster.press_archived(ARCHIVED_TOGGLE));
        f.roster.maintain();
        let seq = archived_asked(&f);
        let mut known = f.bot("Scout");
        known.lifecycle = BotLifecycle::Archived;
        known.held_by = Some(koru_ai_core::bots::BotLifecycleHolder {
            request_id: BotRequestId::mint(),
            kind: BotLifecycleKind::Archive,
            completed_at_ms: 7,
        });
        f.answers
            .send(Answer::Lifecycle {
                seq,
                workspace: f.home,
                lifecycle: BotLifecycle::Archived,
                result: Ok(vec![known.clone()]),
            })
            .unwrap();
        f.roster.maintain();
        assert!(f.roster.unarchive(known.id));
        let request = f
            .commands
            .try_iter()
            .find_map(|command| match command {
                Command::Write { request, .. } => Some(request),
                _ => None,
            })
            .expect("a Restore");
        f.answers
            .send(Answer::Wrote {
                bot: known.id,
                request,
                result,
            })
            .unwrap();
        f.roster.maintain();
        assert_eq!(f.roster.unarchive_refused, Some(words));
        f.roster.maintain();
        assert!(
            f.commands
                .try_iter()
                .any(|command| matches!(command, Command::Lifecycle { .. })),
            "the lists are read again"
        );
    }
}

/// **The keyboard walks a Bot's running and Needs-you sub-rows too**, as
/// the pointer and an assistive technology press them (the second
/// review's item 10): under their Bot's row, in the order drawn, and Enter
/// opens that conversation.
#[test]
fn the_keyboard_walks_the_activity_sub_rows_and_enter_opens_one() {
    let mut f = fixture();
    let (scout, quiet) = (f.bot("Scout"), f.bot("Quiet"));
    f.shown_with(vec![scout.clone(), quiet.clone()]);
    let seq = f.activity_asked();
    let other = koru_ai_core::ChatId::new();
    f.answers
        .send(Answer::Activity {
            seq,
            workspace: f.home,
            result: Ok(activity_page(
                vec![
                    (scout.id, scout.main_chat, BotActivityState::NeedsYou),
                    (scout.id, other, BotActivityState::Running),
                ],
                false,
            )),
        })
        .unwrap();
    f.roster.maintain();
    let stops = f.roster.keyboard_stops();
    let at = |key| stops.iter().position(|stop| *stop == key).expect("a stop");
    let (first, main, running, second) = (
        at(row_key(scout.id)),
        at(activity_key(scout.main_chat)),
        at(activity_key(other)),
        at(row_key(quiet.id)),
    );
    assert!(
        first < main && main < running && running < second,
        "{stops:?}"
    );
    assert!(f.roster.set_focus(scout.id));
    assert!(f.roster.key(Press::Down));
    assert!(f.roster.key(Press::Down));
    assert_eq!(f.roster.focus(), Some(activity_key(other)));
    assert!(f.roster.key(Press::Open));
    let (bot, chat) = f.roster.take_wants_open_chat().expect("asked");
    assert_eq!((bot.id, chat), (scout.id, other));
}

/// **No Unarchive while another operation of the Bot is open**: the owner
/// would refuse it (`refuse_archived`/`bot_command_hold` in
/// `worker/bots/lifecycle.rs`).
#[test]
fn no_unarchive_while_another_operation_is_open() {
    let mut f = fixture();
    f.roster.set_shown(true);
    f.roster.maintain();
    f.asked();
    f.roster.toggle_archived();
    f.roster.maintain();
    let seq = loop {
        match f.commands.try_recv().expect("the Archived filter is asked") {
            Command::Lifecycle { seq, .. } => break seq,
            _ => continue,
        }
    };
    let mut archived = f.bot("Scout");
    archived.lifecycle = BotLifecycle::Archived;
    archived.held_by = Some(koru_ai_core::bots::BotLifecycleHolder {
        request_id: BotRequestId::mint(),
        kind: BotLifecycleKind::Archive,
        completed_at_ms: 1,
    });
    archived.open_operation = Some(koru_ai_core::bots::BotOpenLifecycle {
        request_id: BotRequestId::mint(),
        kind: BotLifecycleKind::Trash,
        chat_id: None,
        stage: koru_ai_core::bots::BotLifecycleStage::Settling,
        refusal: None,
    });
    f.answers
        .send(Answer::Lifecycle {
            seq,
            workspace: f.home,
            lifecycle: BotLifecycle::Archived,
            result: Ok(vec![archived.clone()]),
        })
        .unwrap();
    f.roster.maintain();
    assert!(!f.roster.can_unarchive(archived.id));
    assert!(!f.roster.unarchive(archived.id), "nothing sent");
    assert!(!frame_of(&f.roster).contains(unarchive_key(archived.id)));
}

/// **The roster's keyboard reaches every control it draws**, in the order
/// it draws them: the Bot rows, the Unfinished rows' Retry and Abandon, the
/// Archived toggle, then — open — each archived row and its Unarchive.
/// Enter does what the pointer's press does, through the same functions;
/// the published focus is the control the ring is on.
#[test]
fn the_keyboard_walks_unfinished_and_archived_controls_and_enter_presses_them() {
    let mut f = fixture();
    let scout = f.bot("Scout");
    f.roster.set_shown(true);
    assert!(f.roster.maintain());
    let seq = f.asked();
    let (stuck, ended) = (
        unfinished(f.home, "Writer", true),
        unfinished(f.home, "CFO", false),
    );
    f.answers
        .send(Answer::Listed {
            seq,
            workspace: f.home,
            result: Ok(vec![scout.clone()]),
        })
        .unwrap();
    f.answers
        .send(Answer::Creations {
            seq,
            workspace: f.home,
            result: Ok(Listing::all(vec![stuck.clone(), ended.clone()])),
        })
        .unwrap();
    assert!(f.roster.maintain());
    let _ = f.commands.try_iter().count();
    assert!(f.roster.focus_first());
    let walk = [
        row_key(scout.id),
        creation_retry_key(stuck.request_id),
        creation_abandon_key(stuck.request_id),
        creation_abandon_key(ended.request_id),
        ARCHIVED_TOGGLE,
    ];
    assert_eq!(f.roster.keyboard_stops(), walk);
    for (at, key) in walk.iter().enumerate() {
        assert_eq!(f.roster.focus(), Some(*key), "stop {at}");
        f.roster.key(Press::Down);
    }
    // Every stop is drawn: the keyboard never lands on what is not there.
    let frame = frame_of(&f.roster);
    assert!(walk.iter().all(|key| frame.contains(*key)));
    // Enter on the toggle opens the Archived filter, through its press.
    assert!(f.roster.key(Press::Open));
    assert!(f.roster.archived_open());
    f.roster.maintain();
    let seq = archived_asked(&f);
    let mut archived = f.bot("Older");
    archived.lifecycle = BotLifecycle::Archived;
    let archive = BotRequestId::mint();
    archived.held_by = Some(koru_ai_core::bots::BotLifecycleHolder {
        request_id: archive,
        kind: BotLifecycleKind::Archive,
        completed_at_ms: 7,
    });
    f.answers
        .send(Answer::Lifecycle {
            seq,
            workspace: f.home,
            lifecycle: BotLifecycle::Archived,
            result: Ok(vec![archived.clone()]),
        })
        .unwrap();
    assert!(f.roster.maintain());
    assert_eq!(
        f.roster.focus(),
        Some(ARCHIVED_TOGGLE),
        "kept on the toggle"
    );
    assert!(f.roster.key(Press::Down));
    assert_eq!(f.roster.focus(), Some(archived_key(archived.id)));
    assert!(f.roster.key(Press::Down));
    assert_eq!(f.roster.focus(), Some(unarchive_key(archived.id)));
    // Enter on Unarchive sends the Restore naming the Archive.
    assert!(f.roster.key(Press::Open));
    let sent = f.commands.try_iter().find_map(|command| match command {
        Command::Write { command, .. } => Some(*command),
        _ => None,
    });
    assert!(
        matches!(sent, Some(BotCommand::Restore { source_request_id, .. }) if source_request_id == archive),
        "{sent:?}"
    );
    // Its Unarchive is not drawn while it is on its way: the keyboard
    // lands on a control that is.
    assert_ne!(f.roster.focus(), Some(unarchive_key(archived.id)));
    // Up from the archived row, back through the Unfinished controls; Enter
    // on Retry retries it by its request.
    f.roster.set_focus(scout.id);
    assert!(f.roster.key(Press::Down));
    assert_eq!(f.roster.focus(), Some(creation_retry_key(stuck.request_id)));
    assert!(f.roster.key(Press::Open));
    assert!(
        f.commands.try_iter().any(|command| matches!(
            command,
            Command::Write { command, .. }
                if matches!(*command, BotCommand::RetryBotCreation { request_id } if request_id == stuck.request_id)
        )),
        "Retry went"
    );
}

/// **The Unfinished group never cuts silently**: a listing that stopped at
/// its page limit with the owner holding more says so under the rows.
#[test]
fn unfinished_creations_past_one_listing_say_there_are_more() {
    let mut f = fixture();
    f.roster.set_shown(true);
    assert!(f.roster.maintain());
    let seq = f.asked();
    f.answers
        .send(Answer::Listed {
            seq,
            workspace: f.home,
            result: Ok(Vec::new()),
        })
        .unwrap();
    f.answers
        .send(Answer::Creations {
            seq,
            workspace: f.home,
            result: Ok(Listing {
                items: vec![unfinished(f.home, "Scout", true)],
                more: true,
            }),
        })
        .unwrap();
    assert!(f.roster.maintain());
    assert!(f.roster.creations_more());
    assert!(frame_of(&f.roster).contains(UNFINISHED_MORE));
    for lang in Lang::ALL {
        assert!(!t(lang, Words::BotsUnfinishedMore).trim().is_empty());
    }
}

/// **The activity read is what is running now only while it is fresh**
/// (QUESTIONS O46: the question before an Archive or a Trash sends nothing
/// to find out). Answered by `ListActivity` (`list_activity`,
/// `apps/koru-ai-server/src/worker/bots/reads.rs`, through the real link):
/// it counts; not once a Bot tab's work moved (a read owed), while that
/// read is on its way, once it fails, once the roster is hidden, or once it
/// is older than `ACTIVITY_FRESH`.
#[test]
fn the_activity_counts_as_running_now_only_while_fresh() {
    let mut f = fixture();
    let scout = f.bot("Scout");
    let now = std::time::Instant::now();
    assert!(f.roster.activity_now(now).is_none(), "never read");
    f.shown_with(vec![scout.clone()]);
    let seq = f.activity_asked();
    assert!(f.roster.activity_now(now).is_none(), "on its way");
    assert!(f.roster.take_activity(
        seq,
        f.home,
        Ok(activity_page(
            vec![(scout.id, scout.main_chat, BotActivityState::Running)],
            false
        ))
    ));
    let now = std::time::Instant::now();
    let (entries, truncated) = f.roster.activity_now(now).expect("fresh");
    assert_eq!(entries.len(), 1);
    assert!(!truncated);
    assert!(
        f.roster
            .activity_now(now + ACTIVITY_FRESH + std::time::Duration::from_secs(1))
            .is_none(),
        "too old"
    );
    // A Bot tab saw its work move: the read is owed, so not what runs now.
    f.roster.owe_activity();
    assert!(f.roster.activity_now(now).is_none(), "owed");
    f.roster.maintain();
    let again = f.activity_asked();
    assert!(f.roster.take_activity(again, f.home, Err(Failure::lost())));
    assert_eq!(f.roster.activity().len(), 1, "the rows keep what was shown");
    assert!(
        f.roster.activity_now(now).is_none(),
        "a failed read knows nothing"
    );
    // Hidden: nothing tells it what moved.
    f.roster
        .activity_read_for_test(Vec::new(), false, std::time::Instant::now());
    assert!(f.roster.activity_now(std::time::Instant::now()).is_some());
    f.roster.set_shown(false);
    assert!(
        f.roster.activity_now(std::time::Instant::now()).is_none(),
        "hidden"
    );
}

/// **An archived row's *Unarchive {name}* is published** as a button under
/// its row, pressable — and a roster with nothing unfinished draws no empty
/// Unfinished group (the first window run: an 8 px element under every
/// roster). Listed as `list_lifecycle` answers, `held_by` naming the Archive.
#[test]
fn an_archived_rows_unarchive_is_published_and_no_empty_group_is_drawn() {
    let mut f = fixture();
    f.shown_with(Vec::new());
    let frame = frame_of(&f.roster);
    assert!(
        frame.named("Bots unfinished").next().is_none(),
        "nothing unfinished, no group"
    );
    assert!(f.roster.press_archived(ARCHIVED_TOGGLE));
    f.roster.maintain();
    let seq = archived_asked(&f);
    let mut known = f.bot("Scout");
    known.lifecycle = BotLifecycle::Archived;
    known.held_by = Some(koru_ai_core::bots::BotLifecycleHolder {
        request_id: BotRequestId::mint(),
        kind: BotLifecycleKind::Archive,
        completed_at_ms: 7,
    });
    f.answers
        .send(Answer::Lifecycle {
            seq,
            workspace: f.home,
            lifecycle: BotLifecycle::Archived,
            result: Ok(vec![known.clone()]),
        })
        .unwrap();
    f.roster.maintain();
    let tree = crate::panes::explore::accessibility::roster_tree(&f.roster);
    fn find(
        nodes: &[koru_workspace_host::accessibility::SemanticNode],
        label: &str,
    ) -> Option<koru_workspace_host::accessibility::SemanticNode> {
        nodes.iter().find_map(|node| {
            (node.label.as_deref() == Some(label))
                .then(|| node.clone())
                .or_else(|| find(&node.children, label))
        })
    }
    let unarchive = find(&tree, "Unarchive Scout").expect("Unarchive is published");
    assert_eq!(unarchive.role, accesskit::Role::Button);
    assert!(unarchive.actions.contains(&accesskit::Action::Click));
}
