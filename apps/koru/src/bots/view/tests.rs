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
    /// Its conversation list's reads (S8) are the list's, and are skipped.
    fn asked(&self) -> u64 {
        loop {
            match self.commands.try_recv().expect("a request was sent") {
                Command::Read { seq, bot } => {
                    assert_eq!(bot, self.bot.id, "about this tab's Bot");
                    return seq;
                }
                Command::ListChats { .. } => continue,
                Command::List { .. }
                | Command::Write { .. }
                | Command::Label { .. }
                | Command::Activity { .. }
                | Command::Lifecycle { .. }
                | Command::TrashedChats { .. } => {
                    panic!("a Bot tab only reads its Bot here")
                }
            }
        }
    }

    /// The list read the tab just sent (S8): its sequence number.
    fn listed(&self) -> u64 {
        loop {
            match self.commands.try_recv().expect("a list read was sent") {
                Command::ListChats { seq, bot, .. } => {
                    assert_eq!(bot, self.bot.id, "about this tab's Bot");
                    return seq;
                }
                _ => continue,
            }
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

/// **The header says what the service said, and its one control is
/// *Settings***: the name, *Paused*, where the work goes and the home-only
/// limit, in that nesting under the tab; no line in it is a pointer target.
/// While the Bot is being read it says so, claims no Bot and offers no
/// *Settings*.
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
    // The main conversation is the one shown: the header says so (S8).
    assert_eq!(keys, [NAME, PAUSED, MAIN_MARK, SAVES, LIMIT]);
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
        let targets: Vec<Key> = frame
            .hits()
            .into_iter()
            .map(|hit| hit.key)
            .filter(|key| {
                frame
                    .node(*key)
                    .is_some_and(|node| node.rect.y + node.rect.h <= header.y + header.h + 0.5)
            })
            .collect();
        assert_eq!(
            targets,
            [actions::ACTIONS_BUTTON, region::SETTINGS_BUTTON],
            "the header's controls: the lifecycle's ⋯ (S14), then Settings"
        );
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
    assert!(!f.view.on_conversation(conversation.x + 1.0, header.y + 1.0));
    assert!(
        f.view
            .on_conversation(conversation.x + 1.0, conversation.y + 1.0)
    );
    // Left of it is the conversation column (S8), not the chat.
    assert!(
        conversation.x > 0.0,
        "the column sits left of the conversation"
    );
    assert!(
        !f.view
            .on_conversation(conversation.x - 1.0, conversation.y + 1.0)
    );
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

/// **An archived Bot says so in words beside its name** (P1.7), from the
/// Bot's own `lifecycle`: a line of the header, nested in it, published to
/// a screen reader and no control — never color alone. An active Bot says
/// nothing of it, and a read that finds it active again takes it away.
#[test]
fn an_archived_bot_says_so_in_words_beside_its_name() {
    let mut f = fixture();
    f.view.shown_pass(None);
    let seq = f.asked();
    let mut archived = f.bot.clone();
    archived.lifecycle = BotLifecycle::Archived;
    f.answer(seq, f.bot.id, Ok(archived));
    assert!(f.view.facts().is_some_and(|facts| facts.archived));
    let lines = f.view.header_lines();
    let keys: Vec<Key> = lines.iter().map(|line| line.0).collect();
    // An Archive this window does not know cannot be undone from here,
    // and the header says so (S14).
    assert_eq!(
        keys,
        [
            NAME,
            PAUSED,
            ARCHIVED,
            MAIN_MARK,
            SAVES,
            LIMIT,
            actions::LIFECYCLE_NOTICE
        ]
    );
    assert_eq!(
        lines
            .iter()
            .find(|line| line.0 == ARCHIVED)
            .map(|line| line.2.as_str()),
        Some(t(f.view.lang, Words::BotsArchived)),
        "said in words"
    );
    f.view.with_frame(|_, frame| {
        let title = frame.named("Bot title").next().expect("a title row").rect;
        let mark = frame.node(ARCHIVED).expect("laid out").rect;
        assert!(
            mark.y >= title.y - 0.5 && mark.y + mark.h <= title.y + title.h + 0.5,
            "beside the name, in its row"
        );
        assert_eq!(
            frame.hit_at(mark.x + mark.w * 0.5, mark.y + mark.h * 0.5),
            None,
            "a statement, not a control"
        );
        assert!(frame.named("Bot archived").next().is_some());
    });

    assert!(f.view.ask());
    let again = f.asked();
    f.answer(again, f.bot.id, Ok(f.bot.clone()));
    assert!(f.view.facts().is_some_and(|facts| !facts.archived));
    assert!(
        !f.view.with_frame(|_, frame| frame.contains(ARCHIVED)),
        "active again: not said"
    );
    for lang in Lang::ALL {
        assert!(!t(lang, Words::BotsArchived).trim().is_empty(), "{lang:?}");
    }
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

// ------------------------------------------------------------ settings (S5)

use super::region::*;
use koru_ai_core::bots::{BotAccess, BotCommand, BotOperation, BotOperationKind};
use koru_ai_core::bots::{BotOperationStatus, BotPage, BotRequestId, BotResponse};

impl Fixture {
    /// Shown, answered with the Bot, and its settings open.
    fn opened(bot: Bot) -> Self {
        let mut f = fixture();
        f.bot = bot;
        f.view.bot = Some(f.bot.id);
        f.view.chat = f.bot.main_chat;
        f.view.shown_pass(None);
        let seq = f.asked();
        f.answer(seq, f.bot.id, Ok(f.bot.clone()));
        f.view.set_home_name(Some("Research".into()));
        // Tall enough that every card shows without scrolling: a press is
        // only a press where the region's viewport shows the control.
        f.view.set_viewport(800.0, 1600.0);
        f.view.set_settings_open(true);
        f
    }

    /// The one settings change the tab just sent.
    fn wrote(&self) -> (BotRequestId, BotCommand) {
        loop {
            match self.commands.try_recv().expect("a change was sent") {
                Command::Write {
                    bot,
                    request,
                    command,
                } => {
                    assert_eq!(bot, self.bot.id);
                    return (request, *command);
                }
                // The conversation list's own reads (S8), and the reads a
                // lifecycle answer leaves owed (S14): not writes.
                Command::ListChats { .. } | Command::Read { .. } | Command::Lifecycle { .. } => {
                    continue;
                }
                _ => panic!("not a write"),
            }
        }
    }

    fn reply(&mut self, request: BotRequestId, result: Result<BotResponse, Failure>) {
        self.answers
            .send(Answer::Wrote {
                bot: self.bot.id,
                request,
                result,
            })
            .expect("the tab's end is open");
        self.view.poll();
    }

    fn keys(&self) -> Vec<Key> {
        self.view
            .with_frame(|_, frame| frame.nodes.iter().filter_map(|node| node.key).collect())
    }

    fn press(&mut self, key: Key) -> bool {
        let rect = self
            .view
            .with_frame(|_, frame| frame.node(key).map(|node| node.rect))
            .expect("the control is shown");
        let mut ws = koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t")
            .expect("a document");
        View::mouse_down(
            &mut self.view,
            &mut ws,
            rect.x + rect.w * 0.5,
            rect.y + rect.h * 0.5,
            false,
        )
    }
}

fn unpaused() -> Bot {
    bot(BotId::mint(), WorkspaceId::new(), "Scout", false)
}

fn pending(
    request: BotRequestId,
    bot: &Bot,
    kind: BotOperationKind,
    status: BotOperationStatus,
) -> BotResponse {
    BotResponse::Operation {
        operation: BotOperation {
            request_id: request,
            bot_id: bot.id,
            kind,
            run_id: None,
            status,
        },
    }
}

/// **The settings region is an element tree whose rows nest under their
/// card**: every row's element has a card among its ancestors and lies
/// inside it, every card lies inside the region, and the region sits below
/// the header in the conversation's place.
#[test]
fn the_settings_region_is_an_element_tree_whose_rows_nest_under_their_card() {
    let f = Fixture::opened(unpaused());
    f.view.with_frame(|_, frame| {
        let region = frame.node(SETTINGS).expect("the region").rect;
        let header = frame.node(HEADER).expect("the header").rect;
        assert!(region.y >= header.y + header.h - 0.5, "below the header");
        assert!(
            frame.node(CONVERSATION).is_none(),
            "in the conversation's place"
        );
        let cards: Vec<usize> = frame
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| node.name == "Bot settings card")
            .map(|(index, _)| index)
            .collect();
        assert_eq!(
            cards.len(),
            5,
            "about, agent and model, access, folder, pause"
        );
        for &card in &cards {
            let rect = frame.nodes[card].rect;
            assert!(rect.x >= region.x - 0.5 && rect.x + rect.w <= region.x + region.w + 0.5);
        }
        for row in [
            "Bot name row",
            "Bot role row",
            "Bot model row",
            "Bot model save row",
            "Bot access row",
            "Bot access actions",
            "Bot folder actions",
            "Bot pause row",
        ] {
            let (index, node) = frame
                .nodes
                .iter()
                .enumerate()
                .find(|(_, node)| node.name == row)
                .unwrap_or_else(|| panic!("{row} is laid out"));
            let mut parent = frame.nodes[index].parent;
            let mut card = None;
            while let Some(at) = parent {
                if cards.contains(&at) {
                    card = Some(at);
                    break;
                }
                parent = frame.nodes[at].parent;
            }
            let card = frame.nodes[card.unwrap_or_else(|| panic!("{row} is under a card"))].rect;
            assert!(
                node.rect.x >= card.x - 0.5
                    && node.rect.y >= card.y - 0.5
                    && node.rect.x + node.rect.w <= card.x + card.w + 0.5
                    && node.rect.y + node.rect.h <= card.y + card.h + 0.5,
                "{row} lies inside its card"
            );
        }
        assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
    });
}

/// **Exactly the two approved access choices**, and a press on a segment
/// chooses it — as an unsaved choice: nothing is sent until *Save*.
#[test]
fn the_access_choice_has_exactly_two_segments_and_a_press_drafts_it() {
    let mut f = Fixture::opened(unpaused());
    let choice = f
        .view
        .controls()
        .into_iter()
        .flat_map(|control| control.children)
        .find(|control| control.key == ACCESS_CARD)
        .and_then(|card| card.children.into_iter().find(|c| c.key == ACCESS_CHOICE))
        .expect("the choice is published in the access card");
    assert_eq!(choice.children.len(), 2, "exactly two choices");
    assert!(choice.children.iter().all(|c| c.role == ControlRole::Radio));
    assert_eq!(
        choice
            .children
            .iter()
            .map(|c| c.selected)
            .collect::<Vec<_>>(),
        [Some(true), Some(false)]
    );
    let workspace = choice.children[1].rect;
    let mut ws = koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t")
        .expect("a document");
    assert!(View::mouse_down(
        &mut f.view,
        &mut ws,
        workspace.x + workspace.w * 0.5,
        workspace.y + workspace.h * 0.5,
        false
    ));
    assert_eq!(f.view.settings().access_shown(), BotAccess::Workspace);
    assert!(f.commands.try_recv().is_err(), "nothing sent until Save");
}

/// **Pending states**: access, pause and the folder each show their pending
/// line in place of their control until the owner answers, and nothing is
/// shown as done before it does.
#[test]
fn access_pause_and_folder_show_pending_until_the_owner_answers() {
    let mut f = Fixture::opened(unpaused());
    f.view.perform(Act::Choose(BotAccess::Workspace));
    assert!(f.press(SAVE_ACCESS));
    let (access, _) = f.wrote();
    assert!(f.keys().contains(&ACCESS_PENDING));
    assert!(!f.keys().contains(&SAVE_ACCESS));

    assert!(f.press(PAUSE));
    let (pause, sent) = f.wrote();
    assert!(matches!(sent, BotCommand::Pause { .. }));
    assert!(f.keys().contains(&PAUSE_PENDING) && !f.keys().contains(&PAUSE));
    let bot = f.bot.clone();
    f.reply(
        pause,
        Ok(pending(
            pause,
            &bot,
            BotOperationKind::Pause,
            BotOperationStatus::Pending,
        )),
    );
    assert!(f.keys().contains(&PAUSE_PENDING), "still pending");
    assert!(
        f.view.facts().is_some_and(|facts| !facts.paused),
        "the header follows the owner, not the press"
    );

    let folder = if cfg!(windows) {
        "D:\\bots\\scout"
    } else {
        "/bots/scout"
    };
    assert!(f.view.folder_chosen(std::path::Path::new(folder)));
    let (moved, _) = f.wrote();
    assert!(f.keys().contains(&FOLDER_PENDING) && !f.keys().contains(&CHANGE_FOLDER));

    f.reply(
        access,
        Ok(pending(
            access,
            &bot,
            BotOperationKind::UpdateAccess,
            BotOperationStatus::Accepted,
        )),
    );
    assert!(!f.keys().contains(&ACCESS_PENDING));
    let mut answered = bot.clone();
    answered.config.working_folder = folder.to_owned();
    answered.revision += 1;
    f.reply(moved, Ok(BotResponse::Bot { bot: answered }));
    assert!(f.keys().contains(&CHANGE_FOLDER), "changeable again");
}

/// **Change… is absent while the folder is locked** — a first task's lock
/// begun, or the folder frozen — and the sentence that it is fixed is said
/// in its place.
#[test]
fn change_folder_is_absent_while_the_folder_is_locked() {
    for (lock, frozen) in [(Some(BotRequestId::mint()), None), (None, Some(1))] {
        let mut locked = unpaused();
        locked.folder_lock_request = lock;
        locked.folder_frozen_at_ms = frozen;
        let mut f = Fixture::opened(locked);
        let keys = f.keys();
        assert!(!keys.contains(&CHANGE_FOLDER));
        assert!(keys.contains(&FOLDER_FIXED));
        assert!(!f.view.perform(Act::ChangeFolder));
        assert!(f.view.take_folder_request().is_none());
    }
    let mut f = Fixture::opened(unpaused());
    assert!(f.keys().contains(&CHANGE_FOLDER));
    assert!(!f.keys().contains(&FOLDER_FIXED));
    assert!(f.view.take_folder_request().is_none());
}

/// **The keyboard comes back where it was after the folder dialog**: Enter
/// on *Change…* asks the window for its dialog, and when the dialog answers
/// the keyboard is still on *Change…* — which shows *Saving…* in its place
/// while the owner applies it, and comes back when it has.
#[test]
fn focus_returns_to_change_after_the_folder_dialog() {
    let mut f = Fixture::opened(unpaused());
    let mut ws = koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t")
        .expect("a document");
    while f.view.focus() != Some(CHANGE_FOLDER) {
        assert!(View::key(
            &mut f.view,
            &mut ws,
            &WinitKey::Named(winit::keyboard::NamedKey::Tab),
            None,
            ModifiersState::empty()
        ));
    }
    assert!(View::key(
        &mut f.view,
        &mut ws,
        &WinitKey::Named(winit::keyboard::NamedKey::Enter),
        None,
        ModifiersState::empty()
    ));
    let start = f
        .view
        .take_folder_request()
        .expect("the dialog is asked for");
    assert_eq!(start, Some(PathBuf::from(&f.bot.config.working_folder)));
    let folder = if cfg!(windows) {
        "D:\\bots\\scout"
    } else {
        "/bots/scout"
    };
    assert!(f.view.folder_chosen(std::path::Path::new(folder)));
    assert_eq!(f.view.focus(), Some(CHANGE_FOLDER), "the keyboard stayed");
    let (request, _) = f.wrote();
    let mut answered = f.bot.clone();
    answered.config.working_folder = folder.to_owned();
    f.reply(request, Ok(BotResponse::Bot { bot: answered }));
    assert!(f.view.focus_order().contains(&CHANGE_FOLDER));
    assert_eq!(f.view.focus(), Some(CHANGE_FOLDER));
}

/// **Accessibility names and order**: *Settings* first, then the region as
/// a group named *Settings* holding its cards in reading order, each card
/// named and holding its controls; every control has a name, and the
/// controls a client can press come in the keyboard's order.
#[test]
fn the_controls_publish_names_in_the_keyboards_order() {
    let f = Fixture::opened(unpaused());
    let lang = f.view.lang;
    let roots = f.view.controls();
    assert_eq!(roots.len(), 3, "the ⋯, Settings, then the region");
    assert_eq!(roots[0].key, actions::ACTIONS_BUTTON);
    assert_eq!(roots[0].label, t(lang, Words::BotsActions));
    let roots = &roots[1..];
    assert_eq!(roots[0].key, SETTINGS_BUTTON);
    assert_eq!(roots[0].label, t(lang, Words::BotsSettings));
    assert_eq!(roots[0].selected, Some(true), "the region is open");
    assert_eq!(roots[1].key, SETTINGS);
    assert_eq!(roots[1].label, t(lang, Words::BotsSettings));
    let cards: Vec<Key> = roots[1]
        .children
        .iter()
        .filter(|c| c.role == ControlRole::Group)
        .map(|c| c.key)
        .collect();
    assert_eq!(
        cards,
        [ABOUT_CARD, AGENT_CARD, ACCESS_CARD, FOLDER_CARD, PAUSE_CARD]
    );

    fn walk(controls: &[Control], out: &mut Vec<Control>) {
        for control in controls {
            out.push(control.clone());
            walk(&control.children, out);
        }
    }
    let mut all = Vec::new();
    // Every published control, the ⋯ included.
    walk(&f.view.controls(), &mut all);
    assert!(
        all.iter().all(|c| !c.label.trim().is_empty()),
        "every node is named"
    );
    let field = all
        .iter()
        .find(|c| c.key == NAME_FIELD)
        .expect("the name field");
    assert_eq!(field.role, ControlRole::TextInput);
    assert_eq!(field.value.as_deref(), Some("Scout"));
    let pressable: Vec<Key> = all
        .iter()
        .filter(|c| matches!(c.role, ControlRole::Button | ControlRole::TextInput) && !c.disabled)
        .map(|c| c.key)
        .collect();
    let order: Vec<Key> = f
        .view
        .focus_order()
        .into_iter()
        .filter(|key| *key != ACCESS_CHOICE)
        .collect();
    assert_eq!(pressable, order, "the tree reads in the keyboard's order");
    for key in [RUNS_ON, RUNTIME_LIMITS] {
        let line = all
            .iter()
            .find(|c| c.key == key)
            .expect("the disclosure is published");
        assert_eq!(line.role, ControlRole::Label);
        assert!(
            line.label.contains("Codex"),
            "names the agent: {}",
            line.label
        );
    }
}

/// **The disclosures name the Bot's own agent, whichever it is**
/// (QUESTIONS O33, withdrawn: any runtime may run a Bot) and say nothing
/// about one runtime being the only one.
#[test]
fn the_disclosures_name_the_bots_own_agent() {
    let mut bot = unpaused();
    bot.config.model.provider = AiProviderId::parse("agent-claude").expect("a provider id");
    let f = Fixture::opened(bot);
    assert_eq!(f.view.agent_name(), "Claude");
    let runs = f
        .view
        .controls()
        .into_iter()
        .flat_map(|control| {
            let mut all = vec![control.clone()];
            all.extend(control.children.iter().cloned());
            all.extend(control.children.iter().flat_map(|c| c.children.clone()));
            all
        })
        .find(|control| control.key == RUNS_ON)
        .expect("the disclosure is published");
    assert!(runs.label.contains("Claude"), "{}", runs.label);
    assert!(!runs.label.contains("Codex"), "{}", runs.label);
}

/// **A hidden tab sends nothing, and reads its Bot exactly once when it is
/// shown again** — even with an operation pending whose poll falls due while
/// it is hidden.
#[test]
fn a_hidden_tab_sends_nothing_and_reads_once_when_shown_again() {
    let mut f = Fixture::opened(unpaused());
    assert!(f.view.perform(Act::TogglePause));
    let (pause, _) = f.wrote();
    let bot = f.bot.clone();
    f.reply(
        pause,
        Ok(pending(
            pause,
            &bot,
            BotOperationKind::Pause,
            BotOperationStatus::Pending,
        )),
    );
    f.view.hidden();
    std::thread::sleep(std::time::Duration::from_millis(300));
    let mut ws = koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t")
        .expect("a document");
    View::maintain(&mut f.view, &mut ws, std::time::Instant::now());
    assert!(f.commands.try_recv().is_err(), "a hidden tab sends nothing");

    assert!(f.view.shown_pass(None));
    let mut reads = 0;
    let mut polls = 0;
    while let Ok(command) = f.commands.try_recv() {
        match command {
            Command::Read { .. } => reads += 1,
            Command::Write { command, .. } => {
                assert!(matches!(*command, BotCommand::ReadOperation { .. }));
                polls += 1;
            }
            // The conversation list reads once too (S8).
            Command::ListChats { .. } => {}
            _ => panic!("a tab lists no Bots"),
        }
    }
    assert_eq!(reads, 1, "one read on showing");
    assert_eq!(polls, 1, "and the poll it owed");
    f.view.shown_pass(None);
    assert!(f.commands.try_recv().is_err(), "and only once");
}

/// **Selected pages are added through the shared entity picker and removed
/// by their row's *Remove***: *Add a page…* opens the picker over the home
/// workspace alone, a page picked joins the unsaved access choice and the
/// keyboard comes back to *Add a page…*; *Remove* takes it out again. Neither
/// sends anything before *Save*.
#[test]
fn selected_pages_are_added_through_the_picker_and_removed_by_their_row() {
    use koru_editor::components::entity_picker::{DocumentChoice, WorkspaceChoice};
    let mut f = Fixture::opened(unpaused());
    assert!(f.view.perform(Act::AddPage));
    assert!(f.view.page_picker_open());
    let home = koru_core::WorkspaceId::from_uuid(f.bot.home_workspace.as_uuid());
    let elsewhere = koru_core::WorkspaceId::new();
    let choice = |id| WorkspaceChoice {
        profile: koru_core::ProfileId::new(),
        profile_name: "Me".into(),
        id,
        name: "W".into(),
    };
    f.view
        .set_picker_context(vec![choice(home), choice(elsewhere)]);
    let document = koru_core::DocumentId::new();
    f.view.set_picker_results(
        vec![DocumentChoice {
            target: koru_editor::trigger::DocumentTarget {
                document,
                node: None,
            },
            title: "Sources".into(),
            breadcrumb: String::new(),
            is_file: false,
        }],
        false,
    );
    let mut ws = koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t")
        .expect("a document");
    assert!(View::key(
        &mut f.view,
        &mut ws,
        &WinitKey::Named(winit::keyboard::NamedKey::Enter),
        None,
        ModifiersState::empty()
    ));
    assert!(!f.view.page_picker_open(), "a pick closes the picker");
    assert_eq!(f.view.focus(), Some(ADD_PAGE), "the keyboard came back");
    let page = BotPage {
        workspace: f.bot.home_workspace,
        document: koru_ai_core::DocumentId::from_uuid(document.as_uuid()),
    };
    assert_eq!(f.view.settings().pages_shown(), &[page.clone()]);
    assert!(f.keys().contains(&remove_key(&page)));
    assert!(f.press(remove_key(&page)));
    assert!(f.view.settings().pages_shown().is_empty());
    assert!(f.commands.try_recv().is_err(), "nothing sent before Save");
}

// ------------------------------------------------- keyboard reach and words

/// A key the tab is given, as the window gives it.
fn key(view: &mut BotView, key: WinitKey, text: Option<&str>) -> bool {
    let mut ws = koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t")
        .expect("a document");
    View::key(view, &mut ws, &key, text, ModifiersState::empty())
}

fn named(named: winit::keyboard::NamedKey) -> WinitKey {
    WinitKey::Named(named)
}

/// **`F6`'s tab stop reaches *Settings*, and closing the settings gives the
/// keyboard back where it came from.** Landed on, the header's *Settings*
/// has the keyboard; Enter opens the settings on it; Escape closes them
/// and the keyboard is on *Settings* again; a second Escape gives it to
/// the conversation. Opened by the pointer instead, Escape gives the
/// keyboard straight back to the conversation, where it was.
#[test]
fn the_header_stop_opens_the_settings_and_closing_returns_the_keyboard() {
    use winit::keyboard::NamedKey;
    let mut f = Fixture::opened(unpaused());
    f.view.set_settings_open(false);
    assert_eq!(f.view.focus(), None, "the conversation has the keyboard");
    assert!(f.view.header_stop());
    assert!(!f.view.holds_keyboard());

    assert!(f.view.focus_header());
    assert_eq!(f.view.focus(), Some(SETTINGS_BUTTON));
    assert!(f.view.holds_keyboard());
    assert!(key(&mut f.view, named(NamedKey::Enter), None));
    assert!(f.view.settings_open, "Enter opened the settings");
    assert_eq!(f.view.focus(), Some(SETTINGS_BUTTON));
    assert!(key(&mut f.view, named(NamedKey::Tab), None));
    assert_eq!(f.view.focus(), Some(NAME_FIELD), "into the region");
    assert!(key(&mut f.view, named(NamedKey::Escape), None));
    assert!(!f.view.settings_open);
    assert_eq!(
        f.view.focus(),
        Some(SETTINGS_BUTTON),
        "back on the control that opened them"
    );
    assert!(key(&mut f.view, named(NamedKey::Escape), None));
    assert_eq!(f.view.focus(), None, "then the conversation");

    // The pointer's way: the keyboard was the conversation's.
    assert!(f.press(SETTINGS_BUTTON));
    assert!(f.view.settings_open);
    assert!(key(&mut f.view, named(NamedKey::Escape), None));
    assert!(!f.view.settings_open);
    assert_eq!(f.view.focus(), None, "straight back to the conversation");

    // Released by the window (F6 moved on, or a press elsewhere).
    assert!(f.view.focus_header());
    assert!(f.view.release_keyboard());
    assert_eq!(f.view.focus(), None);
    assert!(!f.view.release_keyboard(), "nothing left to release");
}

/// **No header stop before the tab knows its Bot**: there is no *Settings*
/// to land on, so the rotation skips the tab rather than landing nowhere.
#[test]
fn a_tab_that_does_not_know_its_bot_has_no_header_stop() {
    let mut f = fixture();
    assert!(!f.view.header_stop());
    assert!(!f.view.focus_header());
    assert_eq!(f.view.focus(), None);
}

/// **A Bot's name in any of the eight languages is saved as typed.** Text
/// that arrives with a key — an Arabic or accented layout's letters — is
/// kept byte for byte by the name field, so the owner is sent exactly that
/// name. (Composition through an input method is not delivered to the main
/// window at all; see the Bots design notes §11.)
#[test]
fn a_name_typed_in_any_script_is_saved_as_typed() {
    use winit::keyboard::NamedKey;
    for name in [
        "Investigación",
        "Pesquisa",
        "Recherche",
        "Prüfer",
        "باحث المصادر",
        "资料研究员",
        "資料リサーチャー",
        "Scout مختلط 混合",
    ] {
        let mut f = Fixture::opened(unpaused());
        while f.view.focus() != Some(NAME_FIELD) {
            assert!(key(&mut f.view, named(NamedKey::Tab), None));
        }
        let input = f.view.name_input();
        crate::input_engine::with(|engine| input.borrow_mut().select_all(engine));
        assert!(key(
            &mut f.view,
            WinitKey::Character(name.into()),
            Some(name)
        ));
        assert_eq!(f.view.settings().name_shown(), name);
        assert!(key(&mut f.view, named(NamedKey::Enter), None));
        let (_, sent) = f.wrote();
        let BotCommand::Update { config, .. } = sent else {
            panic!("the name is saved through Update");
        };
        assert_eq!(config.name, name, "byte for byte");
    }
}

// ------------------------------------------------ conversations (S8)

fn bot_page(
    bot: &Bot,
    rows: &[(ChatId, koru_ai_core::bots::BotChatPurpose)],
) -> koru_ai_core::bots::BotChatPage {
    koru_ai_core::bots::BotChatPage {
        chats: rows
            .iter()
            .enumerate()
            .map(|(i, (id, purpose))| koru_ai_core::bots::BotConversation {
                chat: koru_ai_core::Chat {
                    id: *id,
                    title: format!("Conversation {i}"),
                    private: false,
                    current_model: bot.config.model.clone(),
                    branched_from: None,
                    agent_working_folder: None,
                    agent_mode: None,
                    agent_fast: None,
                    pinned_at: None,
                    profile_id: None,
                    bot: None,
                    created_at: koru_ai_core::Timestamp::from_millis(10 - i as i64),
                    updated_at: koru_ai_core::Timestamp::from_millis(10 - i as i64),
                },
                label: koru_ai_core::bots::BotChatLabel {
                    bot_id: bot.id,
                    bot_name: bot.config.name.clone(),
                    purpose: *purpose,
                },
            })
            .collect(),
        next_cursor: None,
    }
}

/// A Bot tab that knows its Bot, shown, its list answered with `rows` —
/// and the main conversation first, as the owner always lists it unless
/// it is in Trash.
fn listed(rows: &[(ChatId, koru_ai_core::bots::BotChatPurpose)]) -> Fixture {
    let mut f = Fixture::opened(unpaused());
    f.view.set_settings_open(false);
    f.view.shown_pass(None);
    let seq = f.listed();
    let mut rows = rows.to_vec();
    if !rows.iter().any(|(chat, _)| *chat == f.bot.main_chat) {
        rows.insert(
            0,
            (f.bot.main_chat, koru_ai_core::bots::BotChatPurpose::Main),
        );
    }
    let page = bot_page(&f.bot, &rows);
    f.answers
        .send(Answer::Chats {
            seq,
            bot: f.bot.id,
            result: Ok(page),
        })
        .unwrap();
    assert!(f.view.poll());
    f
}

/// **A Bot's conversations sit beside the one shown, open only**: read
/// through the tab's own link, the shared list's rows with no pin, ⋯ or
/// Delete; choosing one shows it in the tab, which is found by it from then
/// on; the header marks the main one only while it is the one shown.
#[test]
fn the_tab_lists_its_conversations_and_shows_the_one_chosen() {
    let mut f = Fixture::opened(unpaused());
    let (main, other) = (f.bot.main_chat, ChatId::new());
    f.view.set_settings_open(false);
    f.view.shown_pass(None);
    let seq = f.listed();
    let page = bot_page(
        &f.bot,
        &[
            (main, koru_ai_core::bots::BotChatPurpose::Main),
            (other, koru_ai_core::bots::BotChatPurpose::Additional),
        ],
    );
    f.answers
        .send(Answer::Chats {
            seq,
            bot: f.bot.id,
            result: Ok(page),
        })
        .unwrap();
    assert!(f.view.poll());
    // An active Bot's rows offer one conversation's Trash (S14): the ⋯
    // menu and Delete, never the pin.
    assert_eq!(
        f.view.conversations.surface.row_actions(),
        koru_chats_ui::RowActions {
            pin: false,
            menu: true,
            delete: true
        }
    );
    assert_eq!(f.view.conversations.listed().len(), 2);
    let tree = f.view.with_frame(|_, frame| {
        (
            frame.contains(list::CONVERSATIONS),
            frame.contains(list::NEW_CONVERSATION),
            frame.contains(MAIN_MARK),
        )
    });
    assert_eq!(tree, (true, true, true), "column, New, and the main mark");

    f.view.conversations.surface.ask_open(other);
    assert!(f.view.shown_pass(None));
    assert_eq!(f.view.chat_id(), other, "found by the conversation shown");
    let owned = f
        .view
        .conversation()
        .and_then(AiChatView::owned_conversation)
        .expect("bound");
    assert_eq!(owned.chat, other);
    assert!(
        !f.view.with_frame(|_, frame| frame.contains(MAIN_MARK)),
        "not the main one now"
    );
    let named = f.view.conversation_name(main);
    assert!(
        named.ends_with(t(Lang::En, Words::BotsMainConversation)),
        "{named}"
    );
}

/// ***New conversation* asks the owner and shows what it made**; the list
/// reads again so it joins the rows.
#[test]
fn new_conversation_shows_what_the_owner_made() {
    let mut f = listed(&[]);
    assert!(f.view.perform(region::Act::NewConversation));
    let (request, command) = f.wrote();
    assert!(matches!(command, BotCommand::CreateChat { bot_id, .. } if bot_id == f.bot.id));
    let made = ChatId::new();
    f.reply(
        request,
        Ok(BotResponse::Chat {
            reference: koru_ai_core::bots::BotChatReference {
                bot_id: f.bot.id,
                chat_id: made,
            },
        }),
    );
    f.view.shown_pass(None);
    assert_eq!(f.view.chat_id(), made);
    f.listed();
}

/// ***New conversation* on a Bot archived meanwhile is refused in the
/// person's words** (P1.7's `bot_archived`), the tab stays on its
/// conversation, and the Bot is read again so the header can say it is
/// archived.
#[test]
fn new_conversation_on_an_archived_bot_says_so_and_reads_the_bot() {
    let mut f = listed(&[]);
    let shown = f.view.chat_id();
    assert!(f.view.perform(region::Act::NewConversation));
    let (request, _) = f.wrote();
    f.reply(
        request,
        Err(Failure::Refused(crate::bots::settings::BOT_ARCHIVED.into())),
    );
    assert_eq!(
        f.view.conversations.create_refused(),
        Some(Words::BotsErrArchived)
    );
    f.view.shown_pass(None);
    assert_eq!(f.view.chat_id(), shown, "still on its conversation");
    f.asked();
}

/// **A Branch moves the tab to the branch**, and the list reads again to
/// show it: the owned surface is what the branch rebinds.
#[test]
fn a_branch_moves_the_tab_to_the_branch_and_reads_the_list() {
    let mut f = listed(&[]);
    let branch = ChatId::new();
    let facts = f.view.facts().cloned().expect("known");
    f.view
        .conversation_mut()
        .expect("bound")
        .bind_owned(koru_ai_ui::OwnedConversation {
            chat: branch,
            destination: facts.home,
            provider: facts.provider.clone(),
            offers: BOT_OFFERS,
        });
    f.view.shown_pass(None);
    assert_eq!(f.view.chat_id(), branch);
    f.listed();
    // And a later read of the Bot keeps it there.
    assert!(f.view.ask());
    let seq = f.asked();
    let bot = f.bot.clone();
    f.answer(seq, bot.id, Ok(bot));
    assert_eq!(f.view.chat_id(), branch, "not snapped back to the main one");
}

/// **A hidden tab's list sends nothing; shown again it reads once**, and a
/// failed read keeps the rows.
#[test]
fn a_hidden_tabs_list_reads_once_when_shown_again() {
    let other = ChatId::new();
    let mut f = listed(&[(other, koru_ai_core::bots::BotChatPurpose::Additional)]);
    f.view.hidden();
    let mut ws = koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t")
        .expect("a document");
    View::maintain(&mut f.view, &mut ws, std::time::Instant::now());
    assert!(
        f.commands
            .try_iter()
            .all(|command| !matches!(command, Command::ListChats { .. })),
        "hidden: no list read"
    );
    f.view.shown_pass(None);
    let seq = f.listed();
    f.answers
        .send(Answer::Chats {
            seq,
            bot: f.bot.id,
            result: Err(Failure::Unreachable),
        })
        .unwrap();
    f.view.poll();
    assert_eq!(f.view.conversations.listed().len(), 2, "kept");
    assert_eq!(
        f.view.conversations.notice(),
        Some(Words::BotsConversationsUnavailable)
    );
}

/// The lifecycle half of the tab (S14).
mod agent;
mod create;
mod lifecycle;
