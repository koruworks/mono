// SPDX-License-Identifier: MPL-2.0

//! One Bot conversation, one container: the roster, a chat link, a split and
//! a restart all land on the same Bot tab, and none of them on an ordinary
//! chat in its place.

use koru_ai_ui::ChatId;
use koru_editor::{ChatTarget, OpenWhere, SplitSide};
use koru_store_core::ids::WorkspaceId;
use koru_workspace_host::{Settings, TabRef, View};

use crate::bots::BotView;
use crate::bots::roster::RosterBot;
use crate::mainview::MainView;
use crate::mainview::harness::*;
use crate::mainview::tab::{Tab, TabView};

mod memory;

fn scout() -> RosterBot {
    RosterBot::from(crate::bots::tests::bot(
        koru_ai_core::bots::BotId::mint(),
        WorkspaceId::new(),
        "Scout",
        false,
    ))
}

/// Every tab in every pane that is on `chat`, by kind.
fn on_chat(v: &MainView, chat: ChatId) -> Vec<&'static str> {
    v.panes
        .iter_groups()
        .flat_map(|(_, group)| group.iter_tabs())
        .filter(|tab| tab.view.chat_id() == Some(chat))
        .map(|tab| match &tab.view {
            TabView::Bot(_) => "bot",
            TabView::AiChat(_) => "chat",
            _ => "other",
        })
        .collect()
}

/// **The same ChatId from the roster and from anywhere else focuses one
/// container.** The roster's press opens the Bot tab; a link to its
/// conversation, a search hit (`open_ai_chat_tab_for`) and a second press
/// all come back to that tab instead of making another.
#[test]
fn the_same_chat_id_from_the_roster_and_elsewhere_focuses_one_container() {
    let (mut v, mut w) = container();
    let bot = scout();
    let chat = bot.main_chat;
    assert!(v.open_bot_tab(bot.clone()));
    assert_eq!(on_chat(&v, chat), ["bot"]);
    assert_eq!(v.tabs().active_tab().label, "Scout", "named by its Bot");

    // Somewhere else in front.
    let page = a_document(&mut w.ws, "Elsewhere");
    v.open_node(&w.ws, page);
    assert_ne!(v.tabs().active_tab().view.chat_id(), Some(chat));

    assert!(v.open_ai_chat_tab_for(chat), "a link to the conversation");
    assert_eq!(on_chat(&v, chat), ["bot"], "the Bot tab, not a second one");
    assert_eq!(v.tabs().active_tab().view.chat_id(), Some(chat), "in front");

    v.open_node(&w.ws, page);
    assert!(v.open_chat_where(
        ChatTarget {
            chat: chat.as_uuid(),
            block: None,
        },
        OpenWhere::BackgroundTab,
    ));
    assert!(v.open_bot_tab(bot), "the roster's press again");
    assert_eq!(on_chat(&v, chat), ["bot"]);
}

/// **A Bot's conversation never opens as an ordinary chat** where the window
/// knows it is a Bot's: with no tab on it yet, a link opens the Bot's tab.
/// A conversation the window cannot place, with no label read to ask it
/// through (there is no link under test), opens as an ordinary chat did
/// before the read existed; with one it asks first
/// (`every_door_places_a_conversation_by_its_label`).
#[test]
fn a_known_bot_conversation_opens_as_its_bot_tab_from_a_link() {
    let (mut v, _w) = container();
    let bot = scout();
    let chat = bot.main_chat;
    v.explore.bots_mut().listed_for_test(vec![bot]);
    assert!(v.open_ai_chat_tab_for(chat));
    assert_eq!(on_chat(&v, chat), ["bot"]);
    let TabView::Bot(view) = &v.tabs().active_tab().view else {
        panic!("a Bot tab in front");
    };
    let owned = view
        .conversation()
        .and_then(|chat| chat.owned_conversation())
        .expect("its conversation is bound to the Bot");
    assert_eq!(owned.chat, chat);

    let unknown = ChatId::new();
    assert!(v.open_ai_chat_tab_for(unknown));
    assert_eq!(on_chat(&v, unknown), ["chat"], "not known: an ordinary tab");
}

/// **A split moves the Bot tab rather than doubling it**: one chat, one
/// view of it, whichever pane it is in.
#[test]
fn a_split_moves_the_bot_tab_rather_than_doubling_it() {
    let (mut v, mut w) = container();
    let bot = scout();
    let chat = bot.main_chat;
    v.explore.bots_mut().listed_for_test(vec![bot.clone()]);
    assert!(v.open_bot_tab(bot));
    let page = a_document(&mut w.ws, "Beside it");
    v.open_node(&w.ws, page);
    assert!(v.open_chat_where(
        ChatTarget {
            chat: chat.as_uuid(),
            block: None,
        },
        OpenWhere::Split(SplitSide::Right),
    ));
    assert_eq!(on_chat(&v, chat), ["bot"], "moved, not doubled");
    assert!(
        v.panes.iter_groups().count() >= 2,
        "it went to a pane of its own"
    );
}

/// **A Bot tab survives a restart as a Bot tab**: saved as its Bot and its
/// conversation, back on the same conversation before anything is read,
/// with no conversation started for it — and never as an ordinary chat.
#[test]
fn a_bot_tab_is_restored_as_a_bot_tab() {
    let (mut v, mut w) = on_document();
    let bot = scout();
    let (id, chat) = (bot.id, bot.main_chat);
    assert!(v.open_bot_tab(bot));
    let mut settings = Settings::default();
    v.capture_session(&mut settings);
    assert!(
        settings.tabs.iter().any(|(tab, _)| *tab
            == TabRef::Bot {
                bot: id.as_uuid(),
                chat: chat.as_uuid(),
            }),
        "{:?}",
        settings.tabs
    );

    let mut back = MainView::new();
    back.restore(&w, &settings);
    assert_eq!(on_chat(&back, chat), ["bot"]);
    let restored = back
        .panes
        .iter_groups()
        .flat_map(|(_, group)| group.iter_tabs())
        .find_map(|tab| match &tab.view {
            TabView::Bot(view) => Some(view),
            _ => None,
        })
        .expect("the Bot tab came back");
    assert_eq!(restored.bot(), Some(id));
    assert!(
        restored.conversation().is_none(),
        "nothing is bound until the Bot service answers"
    );
    let _ = &mut w;
}

/// A build that hides Bots drops a saved Bot tab — it never comes back as
/// an ordinary chat, or a blank one.
#[test]
fn a_bot_tab_is_dropped_where_bots_are_hidden() {
    let (_, w) = on_document();
    let settings = Settings {
        tabs: vec![
            (TabRef::Welcome, 0.0),
            (
                TabRef::Bot {
                    bot: uuid::Uuid::new_v4(),
                    chat: uuid::Uuid::new_v4(),
                },
                0.0,
            ),
        ],
        ..Settings::default()
    };
    koru_features::testing::with(
        koru_features::Features {
            agents: false,
            ..koru_features::Features::ALL_ON
        },
        || {
            let mut back = MainView::new();
            back.restore(&w, &settings);
            assert!(
                back.tabs()
                    .iter()
                    .all(|tab| !matches!(tab.view, TabView::Bot(_) | TabView::AiChat(_))),
                "dropped, not degraded"
            );
        },
    );
}

/// **A Bot tab behind another sends nothing and lays nothing out**: the
/// window draws and maintains only the tab in front, and the Bot tab's own
/// work counts stay at zero while it is behind.
#[test]
fn a_bot_tab_behind_another_does_no_work() {
    let (mut v, mut w) = container();
    let bot = scout();
    let chat = bot.main_chat;
    v.tabs_mut().push(Tab::bot(BotView::restored(bot.id, chat)));
    let page = a_document(&mut w.ws, "In front");
    v.open_node(&w.ws, page);
    let now = std::time::Instant::now();
    for _ in 0..3 {
        let _ = View::maintain(&mut v, &mut w.ws, now);
    }
    let work = v
        .panes
        .iter_groups()
        .flat_map(|(_, group)| group.iter_tabs())
        .find_map(|tab| match &tab.view {
            TabView::Bot(view) => Some(view.work()),
            _ => None,
        })
        .expect("the Bot tab is there");
    assert_eq!(work, crate::bots::view::Work::default());
}

/// **A Bot's settings reach a screen reader through the window**, and a
/// client's press reaches the Bot tab: the window publishes the region's
/// access choice as two radio buttons under the tab, and a click on the
/// broader one drafts it — nothing is sent, nothing is applied.
#[test]
fn a_bot_tabs_settings_are_published_and_answer_a_clients_press() {
    use accesskit::{Action, Role};
    use koru_workspace_host::accessibility::{Scope, SemanticNode, ViewSlot};
    let (mut v, mut w) = container();
    let bot = crate::bots::tests::bot(
        koru_ai_core::bots::BotId::mint(),
        WorkspaceId::new(),
        "Scout",
        false,
    );
    assert!(v.open_bot_tab(RosterBot::from(bot.clone())));
    let TabView::Bot(view) = v.active_view() else {
        panic!("the Bot tab is in front");
    };
    view.know_bot_for_test(bot);
    assert!(view.perform(crate::bots::view::region::Act::ToggleSettings));

    fn flatten(nodes: &[SemanticNode], out: &mut Vec<SemanticNode>) {
        for node in nodes {
            out.push(node.clone());
            flatten(&node.children, out);
        }
    }
    let mut all = Vec::new();
    flatten(&v.semantics(ViewSlot::from_raw(1), Scope::Whole), &mut all);
    let radios: Vec<&SemanticNode> = all.iter().filter(|n| n.role == Role::RadioButton).collect();
    assert_eq!(radios.len(), 2, "exactly the two access choices");
    assert!(
        all.iter().any(|n| n.role == Role::TextInput
            && n.label.as_deref()
                == Some(koru_i18n::t(
                    crate::format::lang(),
                    koru_i18n::Key::BotsName
                ))),
        "the name field is published by its name"
    );
    let broader = radios[1].occurrence;
    assert!(v.accessibility_action(&mut w.ws, broader, Action::Click, None));
    let TabView::Bot(view) = v.active_view() else {
        panic!("the Bot tab is still in front");
    };
    assert_eq!(
        view.settings().access_shown(),
        koru_ai_core::bots::BotAccess::Workspace
    );
    assert!(view.settings().access_edited(), "drafted, not applied");
}

// ------------------------------------------- routing by the Bot label (B1)

/// The window's label read, over a link this test holds the other end of.
fn labels_link(
    v: &mut MainView,
) -> (
    std::sync::mpsc::Receiver<crate::bots::link::Command>,
    std::sync::mpsc::SyncSender<crate::bots::link::Answer>,
) {
    let (link, commands, answers) = crate::bots::link::BotsLink::test_pair();
    *v.bot_labels_mut() = crate::bots::labels::ChatLabels::with_link(link);
    (commands, answers)
}

/// The chats the window asked the label read about, in order.
fn asked(commands: &std::sync::mpsc::Receiver<crate::bots::link::Command>) -> Vec<ChatId> {
    commands
        .try_iter()
        .filter_map(|command| match command {
            crate::bots::link::Command::Label { chat } => Some(chat),
            _ => None,
        })
        .collect()
}

fn label_for(bot: &RosterBot) -> koru_ai_core::bots::BotChatLabel {
    koru_ai_core::bots::BotChatLabel {
        bot_id: bot.id,
        bot_name: bot.name.clone(),
        purpose: koru_ai_core::bots::BotChatPurpose::Additional,
    }
}

/// The tab on `chat`, wherever it is.
fn tab_on(v: &MainView, chat: ChatId) -> &Tab {
    v.panes
        .iter_groups()
        .flat_map(|(_, group)| group.iter_tabs())
        .find(|tab| tab.view.chat_id() == Some(chat))
        .expect("a tab is on the chat")
}

/// Open `chat` through one of the doors a chat opens by.
fn open_by(v: &mut MainView, door: &str, chat: ChatId) {
    let target = ChatTarget {
        chat: chat.as_uuid(),
        block: None,
    };
    let opened = match door {
        "link" => v.open_ai_chat_tab_for(chat),
        "here" => v.open_chat_where(target, OpenWhere::SameTab),
        "background" => v.open_chat_where(target, OpenWhere::BackgroundTab),
        "split" => v.open_chat_where(target, OpenWhere::Split(SplitSide::Right)),
        "search" => v.open_ai_chat_tab_at(chat, None),
        _ => unreachable!(),
    };
    assert!(opened, "{door}");
}

/// **Every door opens a conversation the window cannot place as a neutral
/// tab, asks once, and then places it**: a Bot's conversation becomes its
/// Bot's tab on that conversation; an ordinary one becomes the ordinary chat
/// tab in the same place, with the same tab id. Never a guess either way.
#[test]
fn every_door_places_a_conversation_by_its_label() {
    for door in ["link", "here", "background", "split", "search"] {
        for labelled in [true, false] {
            let (mut v, mut w) = container();
            let (commands, answers) = labels_link(&mut v);
            let bot = scout();
            let chat = ChatId::new();
            open_by(&mut v, door, chat);
            assert_eq!(asked(&commands), [chat], "{door}: asked once");
            let tab = tab_on(&v, chat);
            let id = tab.id;
            let TabView::Bot(view) = &tab.view else {
                panic!("{door}: a neutral tab while it is asked");
            };
            assert_eq!(view.bot(), None, "{door}: no Bot claimed early");
            assert!(view.conversation().is_none(), "{door}: no chat started");
            assert_eq!(
                tab.label,
                koru_i18n::t(crate::format::lang(), koru_i18n::Key::ChatsTabNew),
                "{door}: never named a Bot early"
            );
            answers
                .send(crate::bots::link::Answer::Labelled {
                    chat,
                    result: Ok(labelled.then(|| label_for(&bot))),
                })
                .unwrap();
            let _ = View::maintain(&mut v, &mut w.ws, std::time::Instant::now());
            let tab = tab_on(&v, chat);
            assert_eq!(tab.id, id, "{door}: placed where it was");
            match (&tab.view, labelled) {
                (TabView::Bot(view), true) => {
                    assert_eq!(view.bot(), Some(bot.id), "{door}");
                    assert_eq!(view.chat_id(), chat, "{door}: on that conversation");
                    assert_eq!(tab.label, "Scout", "{door}: named by the label");
                }
                (TabView::AiChat(_), false) => {}
                _ => panic!("{door}: labelled {labelled}: the wrong kind of tab"),
            }
            assert_eq!(on_chat(&v, chat).len(), 1, "{door}: one tab");
            assert!(asked(&commands).is_empty(), "{door}: asked nothing more");
        }
    }
}

/// **A conversation the window already knows opens at once**, with nothing
/// asked: a label learned before is a Bot's tab straight away, and a chat
/// learned to be ordinary is an ordinary chat exactly as before.
#[test]
fn a_known_conversation_opens_at_once_and_asks_nothing() {
    let (mut v, _w) = container();
    let (commands, _answers) = labels_link(&mut v);
    let bot = scout();
    let (theirs, mine) = (ChatId::new(), ChatId::new());
    v.bot_labels_mut().learn_bot(theirs, bot.id, "Scout");
    v.bot_labels_mut().learn_ordinary(mine);
    assert!(v.open_ai_chat_tab_for(theirs));
    assert!(v.open_ai_chat_tab_for(mine));
    assert!(asked(&commands).is_empty(), "nothing asked");
    assert_eq!(on_chat(&v, theirs), ["bot"]);
    assert_eq!(on_chat(&v, mine), ["chat"]);
    let TabView::Bot(view) = &tab_on(&v, theirs).view else {
        unreachable!()
    };
    assert_eq!(view.bot(), Some(bot.id));
}

/// **A read that does not answer leaves the tab saying so** — never an
/// ordinary chat in its place — and the tab asks once more the next time
/// it is shown, not on a timer.
#[test]
fn an_unanswered_read_says_so_and_asks_again_when_shown() {
    let (mut v, mut w) = container();
    let (commands, answers) = labels_link(&mut v);
    let chat = ChatId::new();
    assert!(v.open_ai_chat_tab_for(chat));
    assert_eq!(asked(&commands), [chat]);
    answers
        .send(crate::bots::link::Answer::Labelled {
            chat,
            result: Err(crate::bots::link::Failure::lost()),
        })
        .unwrap();
    let now = std::time::Instant::now();
    for _ in 0..3 {
        let _ = View::maintain(&mut v, &mut w.ws, now);
    }
    let TabView::Bot(view) = &tab_on(&v, chat).view else {
        panic!("still the neutral tab, not an ordinary chat");
    };
    assert_eq!(view.opening(), Some(crate::bots::view::Opening::Failed));
    assert!(asked(&commands).is_empty(), "nothing polls");

    // Away and back: one more ask.
    let page = a_document(&mut w.ws, "Elsewhere");
    v.open_node(&w.ws, page);
    let _ = View::maintain(&mut v, &mut w.ws, now);
    assert!(v.open_ai_chat_tab_for(chat), "back to it");
    let _ = View::maintain(&mut v, &mut w.ws, now);
    let _ = View::maintain(&mut v, &mut w.ws, now);
    assert_eq!(asked(&commands), [chat], "asked once more");
}

/// **A saved ordinary-chat tab that turns out to be a Bot's** comes back as
/// that Bot's tab on that conversation — the session wrote it before the
/// window could tell — and a restored ordinary chat is placed as one.
#[test]
fn a_restored_chat_tab_that_is_a_bots_comes_back_as_its_bot_tab() {
    let (_, mut w) = on_document();
    let bot = scout();
    let (theirs, mine) = (ChatId::new(), ChatId::new());
    let settings = Settings {
        tabs: vec![
            (TabRef::Chat(theirs.as_uuid()), 0.0),
            (TabRef::Chat(mine.as_uuid()), 0.0),
        ],
        ..Settings::default()
    };
    let mut back = MainView::new();
    let (commands, answers) = labels_link(&mut back);
    back.restore(&w, &settings);
    let mut sent = asked(&commands);
    sent.sort();
    let mut both = vec![theirs, mine];
    both.sort();
    assert_eq!(sent, both, "each restored chat asked about once");
    for (chat, label) in [(theirs, Some(label_for(&bot))), (mine, None)] {
        answers
            .send(crate::bots::link::Answer::Labelled {
                chat,
                result: Ok(label),
            })
            .unwrap();
    }
    let _ = View::maintain(&mut back, &mut w.ws, std::time::Instant::now());
    assert_eq!(on_chat(&back, theirs), ["bot"]);
    assert_eq!(on_chat(&back, mine), ["chat"]);
    // Saved again, the Bot's is saved as its Bot's tab now.
    let mut again = Settings::default();
    back.capture_session(&mut again);
    assert!(again.tabs.iter().any(|(tab, _)| *tab
        == TabRef::Bot {
            bot: bot.id.as_uuid(),
            chat: theirs.as_uuid(),
        }));
}

/// **Opened again from the roster, a Bot's tab on another of its
/// conversations is brought forward** rather than a second tab opened on its
/// main conversation.
#[test]
fn the_roster_brings_forward_the_bots_tab_on_another_conversation() {
    let (mut v, mut w) = container();
    let bot = scout();
    let other = ChatId::new();
    v.explore.bots_mut().listed_for_test(vec![bot.clone()]);
    v.bot_labels_mut().learn_bot(other, bot.id, "Scout");
    assert!(v.open_ai_chat_tab_for(other));
    let page = a_document(&mut w.ws, "Elsewhere");
    v.open_node(&w.ws, page);
    assert!(v.open_bot_tab(bot.clone()));
    assert_eq!(v.tabs().active_tab().view.chat_id(), Some(other));
    assert!(on_chat(&v, bot.main_chat).is_empty(), "no second tab");
}

/// **A Bot's conversation follows agents, not the Koru AI switch**
/// (QUESTIONS O32): with Koru AI off, the roster's press opens the Bot's tab
/// and a link to a conversation the window knows is a Bot's opens it there;
/// a chat it cannot place opens nothing, as every chat did with Koru AI off.
#[test]
fn a_bots_conversation_opens_with_koru_ai_off() {
    koru_features::testing::with(
        koru_features::Features {
            ai: false,
            ..koru_features::Features::ALL_ON
        },
        || {
            let (mut v, _w) = container();
            let bot = scout();
            let other = ChatId::new();
            v.explore.bots_mut().listed_for_test(vec![bot.clone()]);
            v.bot_labels_mut().learn_bot(other, bot.id, "Scout");
            assert!(v.open_ai_chat_tab_for(other), "a known Bot conversation");
            assert_eq!(on_chat(&v, other), ["bot"]);
            assert!(v.open_bot_tab(bot.clone()), "the roster's press");
            let unknown = ChatId::new();
            assert!(
                !v.open_ai_chat_tab_for(unknown),
                "an unplaced chat, with no label read to ask: nothing"
            );
            assert!(on_chat(&v, unknown).is_empty());
        },
    );
}

/// **A link to a chat, for every combination of Koru AI and agents, with a
/// Bot's conversation and with an ordinary chat** (QUESTIONS O32). The
/// window decides by the chat's Bot label:
///
/// - both on: a neutral tab while it asks, then the Bot's tab or the
///   ordinary chat tab in its place;
/// - Koru AI alone: Bots are hidden, nothing is asked, and every chat opens
///   as an ordinary chat tab, as before Bots;
/// - agents alone: **nothing is shown while it asks** — no tab that could
///   turn out to be an ordinary chat — then a Bot's conversation opens as
///   its Bot's tab through the door it was asked by, and an ordinary chat
///   is dropped exactly as every chat was with Koru AI off;
/// - both off: nothing is asked and nothing opens.
#[test]
fn a_chat_link_opens_by_its_label_for_every_combination_of_ai_and_agents() {
    for ai in [false, true] {
        for agents in [false, true] {
            for labelled in [true, false] {
                for door in ["link", "background", "split"] {
                    let features = koru_features::Features {
                        ai,
                        agents,
                        ..koru_features::Features::ALL_ON
                    };
                    koru_features::testing::with(features, || {
                        let case = format!("ai {ai}, agents {agents}, bot {labelled}, {door}");
                        let (mut v, mut w) = container();
                        let (commands, answers) = labels_link(&mut v);
                        let bot = scout();
                        let chat = ChatId::new();
                        let target = ChatTarget {
                            chat: chat.as_uuid(),
                            block: None,
                        };
                        let opened = match door {
                            "link" => v.open_ai_chat_tab_for(chat),
                            "background" => v.open_chat_where(target, OpenWhere::BackgroundTab),
                            _ => v.open_chat_where(target, OpenWhere::Split(SplitSide::Right)),
                        };
                        let asks = agents.then_some(chat).into_iter().collect::<Vec<_>>();
                        assert_eq!(asked(&commands), asks, "{case}: asked");
                        match (ai, agents) {
                            (true, true) => {
                                assert!(opened, "{case}");
                                assert_eq!(on_chat(&v, chat), ["bot"], "{case}: neutral");
                            }
                            (true, false) => {
                                assert!(opened, "{case}");
                                assert_eq!(on_chat(&v, chat), ["chat"], "{case}");
                            }
                            (false, _) => {
                                assert!(!opened, "{case}: nothing yet");
                                assert!(on_chat(&v, chat).is_empty(), "{case}: no tab");
                            }
                        }
                        if agents {
                            answers
                                .send(crate::bots::link::Answer::Labelled {
                                    chat,
                                    result: Ok(labelled.then(|| label_for(&bot))),
                                })
                                .unwrap();
                        }
                        let _ = View::maintain(&mut v, &mut w.ws, std::time::Instant::now());
                        let expected: &[&str] = match (ai, agents, labelled) {
                            (true, true, true) => &["bot"],
                            (true, _, _) => &["chat"],
                            (false, true, true) => &["bot"],
                            (false, _, _) => &[],
                        };
                        assert_eq!(on_chat(&v, chat), expected, "{case}: placed");
                        if labelled && agents && !expected.is_empty() {
                            let TabView::Bot(view) = &tab_on(&v, chat).view else {
                                panic!("{case}: the Bot's tab");
                            };
                            assert_eq!(view.bot(), Some(bot.id), "{case}");
                        }
                        let _ = View::maintain(&mut v, &mut w.ws, std::time::Instant::now());
                        assert!(asked(&commands).is_empty(), "{case}: asked once");
                        assert_eq!(on_chat(&v, chat), expected, "{case}: opened once");
                    });
                }
            }
        }
    }
}

/// **With Koru AI off, a chat link whose read goes unanswered is kept, not
/// dropped** — and not shown either, since it might be an ordinary chat.
/// It is asked about once more when the window is next activated, or when
/// the link is followed again, never on a timer; it opens when the answer is
/// a Bot's, and is dropped only when the chat is ordinary or does not exist.
/// A second link to a chat already being asked about asks nothing more.
#[test]
fn with_koru_ai_off_an_unanswered_link_is_kept_and_asked_again_on_activation() {
    koru_features::testing::with(
        koru_features::Features {
            ai: false,
            ..koru_features::Features::ALL_ON
        },
        || {
            let (mut v, mut w) = container();
            let (commands, answers) = labels_link(&mut v);
            let now = std::time::Instant::now();
            let bot = scout();
            let unanswered = |chat| crate::bots::link::Answer::Labelled {
                chat,
                result: Err(crate::bots::link::Failure::lost()),
            };

            // A Bot's conversation: unanswered, kept, asked again on activation.
            let theirs = ChatId::new();
            assert!(!v.open_ai_chat_tab_for(theirs));
            assert!(!v.open_ai_chat_tab_for(theirs), "followed again meanwhile");
            assert_eq!(asked(&commands), [theirs], "asked once");
            answers.send(unanswered(theirs)).unwrap();
            for _ in 0..3 {
                let _ = View::maintain(&mut v, &mut w.ws, now);
            }
            assert!(on_chat(&v, theirs).is_empty(), "not shown");
            assert!(asked(&commands).is_empty(), "nothing polls");
            View::set_window_focused(&mut v, true);
            View::set_window_focused(&mut v, true);
            assert_eq!(asked(&commands), [theirs], "asked once more on activation");
            answers
                .send(crate::bots::link::Answer::Labelled {
                    chat: theirs,
                    result: Ok(Some(label_for(&bot))),
                })
                .unwrap();
            let _ = View::maintain(&mut v, &mut w.ws, now);
            assert_eq!(on_chat(&v, theirs), ["bot"], "a Bot's: it opens");

            // An ordinary one: unanswered, followed again, then not found.
            let missing = ChatId::new();
            assert!(!v.open_ai_chat_tab_for(missing));
            assert_eq!(asked(&commands), [missing]);
            answers.send(unanswered(missing)).unwrap();
            let _ = View::maintain(&mut v, &mut w.ws, now);
            assert!(!v.open_ai_chat_tab_for(missing), "followed again");
            assert_eq!(asked(&commands), [missing], "asked at once");
            answers
                .send(crate::bots::link::Answer::Labelled {
                    chat: missing,
                    result: Err(crate::bots::link::Failure::Refused("not_found".into())),
                })
                .unwrap();
            let _ = View::maintain(&mut v, &mut w.ws, now);
            assert!(on_chat(&v, missing).is_empty(), "not found: dropped");
            View::set_window_focused(&mut v, true);
            assert!(asked(&commands).is_empty(), "nothing is held any more");
            assert!(
                !v.open_ai_chat_tab_for(missing),
                "known ordinary: nothing, and nothing asked"
            );
            assert!(asked(&commands).is_empty());
        },
    );
}

/// **Create a bot opens one create tab**, found by no conversation, and
/// brought forward rather than doubled while nothing was sent from it; it is
/// **not saved with the session**, so it never comes back as a half-filled
/// form with identities that may no longer be live — a creation that was
/// sent comes back as the roster's unfinished row instead.
#[test]
fn create_a_bot_opens_one_form_found_by_no_conversation_and_not_saved() {
    let (mut v, _w) = on_document();
    let before = v.tabs().len();
    assert!(v.open_bot_create_tab());
    assert_eq!(v.tabs().len(), before + 1);
    let TabView::Bot(view) = &v.tabs().active_tab().view else {
        panic!("a Bot tab in front");
    };
    assert!(view.is_creating());
    assert_eq!(
        v.tabs().active_tab().view.chat_id(),
        None,
        "no conversation"
    );
    assert_eq!(
        v.tabs().active_tab().label,
        koru_i18n::t(crate::format::lang(), koru_i18n::Key::BotsNewBot)
    );
    assert!(v.open_bot_create_tab(), "asked again");
    assert_eq!(v.tabs().len(), before + 1, "the same form, brought forward");
    let mut settings = Settings::default();
    v.capture_session(&mut settings);
    assert!(
        !settings
            .tabs
            .iter()
            .any(|(tab, _)| matches!(tab, TabRef::Bot { .. } | TabRef::Chat(_))),
        "{:?}",
        settings.tabs
    );
}

// ------------------------------------ the Unfinished group from the window

/// An unfinished creation of `home`, as the owner answers it: stopped
/// because the managed folder could not be made (`FolderUnavailable`,
/// retryable), as the second window run produced it.
fn stuck(home: WorkspaceId) -> koru_ai_core::bots::BotCreation {
    koru_ai_core::bots::BotCreation {
        request_id: koru_ai_core::bots::BotRequestId::mint(),
        bot_id: koru_ai_core::bots::BotId::mint(),
        home_workspace: home,
        name: "Dark Check".into(),
        stage: koru_ai_core::bots::BotCreationStage::Incomplete,
        phase: koru_ai_core::bots::BotCreationPhase::RolePending,
        reason: Some(koru_ai_core::bots::BotCreationReason::FolderUnavailable),
        retryable: true,
        role_page: None,
        role_page_unconfirmed: false,
        folder_binding: None,
        left_behind: None,
        bot: None,
    }
}

/// The roster shown in the column over a link the test answers, listed
/// with no Bots and `creations` unfinished — as the link's thread answers
/// a `List` (its second answer, `ListBotCreations`). Its workspace is the
/// window's.
fn roster_with(
    v: &mut MainView,
    creations: impl FnOnce(WorkspaceId) -> Vec<koru_ai_core::bots::BotCreation>,
) -> (
    WorkspaceId,
    std::sync::mpsc::Receiver<crate::bots::link::Command>,
    std::sync::mpsc::SyncSender<crate::bots::link::Answer>,
) {
    use crate::bots::link::{Answer, BotsLink, Command, Listing};
    if !v.sidebar.is_open() {
        assert!(v.toggle_sidebar());
    }
    let (link, commands, answers) = BotsLink::test_pair();
    *v.explore.bots_mut() = crate::bots::BotsRoster::with_link(link);
    v.explore.show_bots();
    v.maintain_bots();
    let (seq, home) = commands
        .try_iter()
        .find_map(|command| match command {
            Command::List { seq, workspace } => Some((seq, workspace)),
            _ => None,
        })
        .expect("the roster lists the window's workspace");
    answers
        .send(Answer::Listed {
            seq,
            workspace: home,
            result: Ok(Vec::new()),
        })
        .unwrap();
    answers
        .send(Answer::Creations {
            seq,
            workspace: home,
            result: Ok(Listing::all(creations(home))),
        })
        .unwrap();
    v.maintain_bots();
    (home, commands, answers)
}

/// Every Bot tab of the window runs its shown pass, as the window's draw
/// gives the tab in front.
fn pass_bot_tabs(v: &mut MainView) {
    for (_, group) in v.panes.iter_groups_mut() {
        for tab in group.tabs_mut().iter_mut() {
            if let TabView::Bot(view) = &mut tab.view {
                view.shown_pass(None);
            }
        }
    }
}

/// **`F6` puts the keyboard in the roster, and its own keys press an
/// Unfinished row's Retry** — entered as a real key enters the window
/// (`MainView::key`), because the unit tests that walked the roster's keys
/// passed while the window offered no way in (the second window run's
/// finding 3c: the arrows walked the rail, and `F6` had no stop on the
/// roster). Retry is sent as the roster sends it, by the request alone.
#[test]
fn f6_puts_the_keyboard_in_the_roster_and_enter_presses_retry() {
    use crate::bots::link::Command;
    use winit::keyboard::{Key, ModifiersState, NamedKey};
    let (mut v, mut w) = on_document();
    let mut listed = None;
    let (_, commands, _answers) = roster_with(&mut v, |home| {
        let one = stuck(home);
        listed = Some(one.clone());
        vec![one]
    });
    let creation = listed.expect("listed");
    assert_eq!(v.explore.bots().creations().len(), 1);
    v.explore.clear_focus();
    let mut press = |v: &mut MainView, key: NamedKey| {
        v.key(&mut w.ws, &Key::Named(key), None, ModifiersState::empty())
    };
    for _ in 0..8 {
        if v.explore.bots().has_keyboard() {
            break;
        }
        press(&mut v, NamedKey::F6);
    }
    assert!(v.explore.bots().has_keyboard(), "F6 reaches the roster");
    let retry = crate::bots::roster::creation_retry_key(creation.request_id);
    for _ in 0..8 {
        if v.explore.bots().focus() == Some(retry) {
            break;
        }
        press(&mut v, NamedKey::ArrowDown);
    }
    assert_eq!(v.explore.bots().focus(), Some(retry), "the arrows walk it");
    let _ = commands.try_iter().count();
    press(&mut v, NamedKey::Enter);
    assert!(
        commands.try_iter().any(|command| matches!(
            command,
            Command::Write { request, .. } if request == creation.request_id
        )),
        "Enter retried it"
    );
}

/// **A create tab's creation that stopped unfinished reaches the roster's
/// Unfinished group at once**, through the window's pass over its tabs —
/// not when the roster is next shown (the second window run's finding 3a).
/// The tab's answer is the owner's own (`CreateBotWithRole`'s creation,
/// stage `Incomplete`).
#[test]
fn a_create_tabs_unfinished_creation_reaches_the_roster_at_once() {
    use crate::bots::link::{Answer, BotsLink};
    let (mut v, _w) = on_document();
    let (home, _commands, _answers) = roster_with(&mut v, |_| Vec::new());
    let (link, _tab_commands, tab_answers) = BotsLink::test_pair();
    let view = BotView::creating_with_link(home, link);
    let form = view.create_form().expect("a create tab");
    let mut creation = stuck(home);
    creation.request_id = form.request();
    creation.bot_id = form.bot();
    v.tabs_mut().push(Tab::bot(view));
    tab_answers
        .send(Answer::Wrote {
            bot: creation.bot_id,
            request: creation.request_id,
            result: Ok(koru_ai_core::bots::BotResponse::Creation {
                creation: creation.clone(),
            }),
        })
        .unwrap();
    pass_bot_tabs(&mut v);
    assert!(v.refresh_bot_tabs());
    assert_eq!(v.explore.bots().creations(), [creation]);
}

/// **A Retry from the roster's Unfinished group is followed by the create
/// tab holding that creation**: it reads its creation again, rather than
/// go on offering Retry and Abandon (the second window run's finding 3d).
#[test]
fn a_retry_from_the_roster_makes_the_create_tab_read_its_creation() {
    use crate::bots::link::{Answer, BotsLink, Command};
    use koru_ai_core::bots::{BotCommand, BotCreationStage, BotResponse};
    let (mut v, _w) = on_document();
    let (home, commands, answers) = roster_with(&mut v, |_| Vec::new());
    let (link, tab_commands, tab_answers) = BotsLink::test_pair();
    let view = BotView::creating_with_link(home, link);
    let form = view.create_form().expect("a create tab");
    let mut creation = stuck(home);
    creation.request_id = form.request();
    creation.bot_id = form.bot();
    v.tabs_mut().push(Tab::bot(view));
    // The tab heard the creation stop, and the roster lists it.
    tab_answers
        .send(Answer::Wrote {
            bot: creation.bot_id,
            request: creation.request_id,
            result: Ok(BotResponse::Creation {
                creation: creation.clone(),
            }),
        })
        .unwrap();
    pass_bot_tabs(&mut v);
    v.explore.bots_mut().know_creation(creation.clone());
    let _ = tab_commands.try_iter().count();
    // Retry from the roster; the owner finished it.
    let _ = commands.try_iter().count();
    assert!(v.explore.bots_mut().retry(creation.request_id));
    assert!(commands.try_iter().count() > 0);
    let mut done = creation.clone();
    done.stage = BotCreationStage::Completed;
    answers
        .send(Answer::Wrote {
            bot: creation.bot_id,
            request: creation.request_id,
            result: Ok(BotResponse::Creation { creation: done }),
        })
        .unwrap();
    v.maintain_bots();
    pass_bot_tabs(&mut v);
    assert!(
        tab_commands.try_iter().any(|command| matches!(
            command,
            Command::Write { command, .. }
                if *command == BotCommand::ReadBotCreation { request_id: creation.request_id }
        )),
        "the create tab reads its creation again"
    );
}

/// **An archived Bot's tab stays archived when its row in the roster's
/// Archived section is clicked** (the fourth window run's finding A: the
/// header lost *Archived* and *Unarchive*, *Bot actions* offered *Archive
/// bot* again and the rows offered *Rename…*, while the roster and the tab's
/// notice still said archived). Entered as a person enters it: the row is
/// found where the window publishes it and clicked there (`View::mouse_down`
/// and `mouse_up`), and the window's own pass hands the roster's ask to the
/// tab. The Archived filter answers as `list_lifecycle` does
/// (`apps/koru-ai-server/src/worker/bots/reads.rs`): the Bot, archived,
/// with the Archive that holds it.
#[test]
fn an_archived_bots_tab_stays_archived_when_its_archived_row_is_clicked() {
    use crate::bots::lifecycle::Ask;
    use crate::bots::link::{Answer, BotsLink, Command};
    use koru_ai_core::bots::{
        BotId, BotLifecycle, BotLifecycleHolder, BotLifecycleKind, BotRequestId,
    };
    use koru_workspace_host::accessibility::{Scope, SemanticNode, ViewSlot};
    fn flatten(nodes: &[SemanticNode], out: &mut Vec<SemanticNode>) {
        for node in nodes {
            out.push(node.clone());
            flatten(&node.children, out);
        }
    }

    let (mut v, mut w) = on_document();
    koru_workspace_host::View::set_viewport(&mut v, 1400.0, 800.0);
    let (home, commands, answers) = roster_with(&mut v, |_| Vec::new());
    let holder = BotRequestId::mint();
    let mut bot = crate::bots::tests::bot(BotId::mint(), home, "Charlie", false);
    bot.lifecycle = BotLifecycle::Archived;
    bot.held_by = Some(BotLifecycleHolder {
        request_id: holder,
        kind: BotLifecycleKind::Archive,
        completed_at_ms: 1,
    });

    // Its tab, archived by its own read (`read`'s answer), in front.
    let (link, tab_commands, tab_answers) = BotsLink::test_pair();
    let mut view = BotView::with_link(bot.id, bot.main_chat, link);
    view.shown_pass(None);
    let read = tab_commands
        .try_iter()
        .find_map(|command| match command {
            Command::Read { seq, .. } => Some(seq),
            _ => None,
        })
        .expect("the tab reads its Bot");
    tab_answers
        .send(Answer::Read {
            seq: read,
            bot: bot.id,
            result: Ok(bot.clone()),
        })
        .unwrap();
    view.shown_pass(None);
    assert_eq!(view.lifecycle_rows(), [Ask::Unarchive, Ask::Trash]);
    v.tabs_mut().push(Tab::bot(view));

    // The roster's Archived section, open and listed.
    v.explore.bots_mut().toggle_archived();
    v.maintain_bots();
    let seq = commands
        .try_iter()
        .find_map(|command| match command {
            Command::Lifecycle {
                seq,
                lifecycle: BotLifecycle::Archived,
                ..
            } => Some(seq),
            _ => None,
        })
        .expect("the Archived filter is asked");
    answers
        .send(Answer::Lifecycle {
            seq,
            workspace: home,
            lifecycle: BotLifecycle::Archived,
            result: Ok(vec![bot.clone()]),
        })
        .unwrap();
    v.maintain_bots();
    assert_eq!(v.explore.bots().archived().len(), 1);

    // Drawn, then clicked where it is published.
    v.relayout();
    v.draw_frame(
        &mut vello::Scene::new(),
        &w,
        &koru_draw::ImageCache::new(),
        vello::kurbo::Affine::IDENTITY,
        false,
        (-1.0, -1.0),
    );
    let mut published = Vec::new();
    flatten(
        &v.semantics(ViewSlot::from_raw(1), Scope::Whole),
        &mut published,
    );
    let row = published
        .iter()
        .find(|node| {
            node.role == accesskit::Role::ListItem && node.label.as_deref() == Some("Charlie")
        })
        .expect("the archived row is published");
    let bounds = row.bounds.expect("with bounds");
    let (x, y) = (bounds.x + bounds.w * 0.5, bounds.y + bounds.h * 0.5);
    View::mouse_down(&mut v, &mut w.ws, x, y, false);
    View::mouse_up(&mut v, &mut w.ws);
    v.maintain_bots();

    let TabView::Bot(view) = &v.tabs().active_tab().view else {
        panic!("the Bot's tab in front");
    };
    assert_eq!(view.bot(), Some(bot.id), "the same tab, brought forward");
    assert!(
        view.facts().is_some_and(|facts| facts.archived),
        "the tab still says archived"
    );
    assert_eq!(
        view.lifecycle_rows(),
        [Ask::Unarchive, Ask::Trash],
        "Bot actions offer Unarchive, never Archive"
    );
    assert_eq!(
        view.rename_refusal(bot.main_chat),
        Some(koru_i18n::Key::BotsErrArchived),
        "its rows offer no rename"
    );
    assert_eq!(on_chat(&v, bot.main_chat), ["bot"], "nothing opened twice");
}

/// **An archived Bot opened from its Archived row with no tab open says it
/// is archived from the start, and offers Unarchive by the Archive the row
/// names** — not as an active Bot until its own read answers.
#[test]
fn an_archived_row_opens_a_tab_that_is_archived_from_the_start() {
    use crate::bots::lifecycle::Ask;
    use koru_ai_core::bots::{
        BotId, BotLifecycle, BotLifecycleHolder, BotLifecycleKind, BotRequestId,
    };
    let mut bot = crate::bots::tests::bot(BotId::mint(), WorkspaceId::new(), "Charlie", false);
    bot.lifecycle = BotLifecycle::Archived;
    bot.held_by = Some(BotLifecycleHolder {
        request_id: BotRequestId::mint(),
        kind: BotLifecycleKind::Archive,
        completed_at_ms: 1,
    });
    let row = RosterBot::from(bot.clone());
    let mut view = BotView::opened(&row);
    assert!(view.facts().is_some_and(|facts| facts.archived));
    // The settings come with the tab's own read; until then the ⋯ waits.
    view.know_bot_for_test(bot);
    assert_eq!(view.lifecycle_rows(), [Ask::Unarchive, Ask::Trash]);
}

/// **Roster rows scrolled out of view are published inside the list's
/// viewport** (the fourth window run, at 940 × 240: after the wheel a row
/// scrolled away was still published over the column's title, and unscrolled
/// the *Archived* heading under the Trash row, each found there by a point).
/// Scrolled as a person scrolls it — the pointer over the list, the wheel
/// through `View::scroll_by` — every Bot row the window publishes lies
/// within the list's own bounds; one scrolled wholly away keeps an empty box
/// at its edge.
#[test]
fn roster_rows_scrolled_out_of_view_are_published_inside_the_list() {
    use koru_workspace_host::accessibility::{Scope, SemanticNode, ViewSlot};
    fn flatten(nodes: &[SemanticNode], out: &mut Vec<SemanticNode>) {
        for node in nodes {
            out.push(node.clone());
            flatten(&node.children, out);
        }
    }
    let features = koru_features::Features {
        agents: true,
        ai: true,
        ..koru_features::Features::ALL_OFF
    };
    koru_features::testing::with(features, || {
        let (mut v, w) = on_document();
        View::set_viewport(&mut v, 940.0, 240.0);
        let home = WorkspaceId::new();
        let bots: Vec<RosterBot> = (0..8)
            .map(|i| {
                RosterBot::from(crate::bots::tests::bot(
                    koru_ai_core::bots::BotId::mint(),
                    home,
                    &format!("Bot {i}"),
                    false,
                ))
            })
            .collect();
        v.explore.bots_mut().listed_for_test(bots);
        v.show_navigator(crate::panes::explore::NavigatorMode::Bots);
        if !v.sidebar.is_open() {
            assert!(v.toggle_sidebar());
        }
        v.sidebar
            .advance(std::time::Instant::now() + std::time::Duration::from_secs(5));
        let frame = |v: &mut MainView| {
            v.relayout();
            v.draw_frame(
                &mut vello::Scene::new(),
                &w,
                &koru_draw::ImageCache::new(),
                vello::kurbo::Affine::IDENTITY,
                false,
                (-1.0, -1.0),
            );
            let mut published = Vec::new();
            flatten(
                &v.semantics(ViewSlot::from_raw(1), Scope::Whole),
                &mut published,
            );
            published
        };
        let title = koru_i18n::t(crate::format::lang(), koru_i18n::Key::BotsTitle);
        let list = |published: &[SemanticNode]| {
            published
                .iter()
                .find(|node| {
                    node.role == accesskit::Role::List && node.label.as_deref() == Some(title)
                })
                .cloned()
                .expect("the roster's list is published")
        };
        let published = frame(&mut v);
        let band = list(&published).bounds.expect("with bounds");
        View::mouse_move(&mut v, band.x + band.w * 0.5, band.y + band.h * 0.5);
        View::scroll_by(&mut v, winit::event::MouseScrollDelta::LineDelta(0.0, -6.0));
        let published = frame(&mut v);
        assert!(
            v.explore.bots().scroll_offset() > 0.0,
            "the wheel scrolled the roster"
        );
        let list = list(&published);
        let band = list.bounds.expect("with bounds");
        let rows: Vec<_> = list
            .children
            .iter()
            .filter(|node| node.role == accesskit::Role::ListItem)
            .collect();
        assert_eq!(rows.len(), 8, "every Bot is published");
        for row in &rows {
            let r = row.bounds.expect("with bounds");
            assert!(
                r.y >= band.y - 0.5 && r.y + r.h <= band.y + band.h + 0.5,
                "{:?} at {r:?} outside the list {band:?}",
                row.label
            );
        }
        assert!(
            rows.iter()
                .any(|row| row.bounds.is_some_and(|r| r.h <= 0.0)),
            "a row scrolled wholly away keeps an empty box"
        );
    });
}
