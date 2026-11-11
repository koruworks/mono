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
            result: Err(crate::bots::link::Failure::Unreachable),
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
                result: Err(crate::bots::link::Failure::Unreachable),
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
