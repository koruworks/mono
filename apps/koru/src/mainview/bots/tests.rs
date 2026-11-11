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
/// A conversation the window cannot place stays an ordinary tab — the window
/// does not guess (the engine's `Chat.bot` label is not built).
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
    assert_eq!(restored.bot(), id);
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
