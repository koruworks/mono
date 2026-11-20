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

#[test]
fn an_authoritatively_idle_open_chat_clears_its_old_activity_mark() {
    use koru_chats_ui::ChatStatus;
    let mut f = listed(&[]);
    let chat = f.bot.main_chat;
    let other = ChatId::new();
    let activity = std::collections::HashMap::from([
        (chat, ChatStatus::Working),
        (other, ChatStatus::Working),
    ]);
    f.view.set_activity(&activity);
    assert_eq!(
        f.view.conversations.surface.status(chat),
        ChatStatus::Working
    );
    f.view.conversation_mut().unwrap().pretend_read(chat);
    assert!(f.view.set_activity(&activity));
    assert_eq!(f.view.conversations.surface.status(chat), ChatStatus::Idle);
    assert_eq!(
        f.view.conversations.surface.status(other),
        ChatStatus::Working
    );
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
                Command::ListChats { .. } | Command::ReadLibrary { .. } => continue,
                Command::List { .. }
                | Command::RoutineStartup { .. }
                | Command::NativeRoutineControl { .. }
                | Command::RoutineControl { .. }
                | Command::ReadRoutines { .. }
                | Command::ReadRoutineDetail { .. }
                | Command::RoutineLabel { .. }
                | Command::Write { .. }
                | Command::Label { .. }
                | Command::Activity { .. }
                | Command::TeamPause { .. }
                | Command::SetTeamPause { .. }
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
        let changed = self.view.poll();
        self.publishable("after the Bot's read");
        changed
    }

    /// The tab's published tree is one AccessKit can take: no node twice
    /// (`crate::bots::tests::assert_publishable`).
    fn publishable(&self, stage: &str) {
        crate::bots::tests::assert_bot_tab_publishable(stage, &self.view);
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
    assert_eq!(owned.refusals, BOT_REFUSALS, "the Bot's words, handed in");

    // Asked again; the answer is a failure.
    assert!(f.view.ask());
    let again = f.asked();
    assert!(f.answer(again, f.bot.id, Err(Failure::lost())));
    assert_eq!(f.view.load(), Load::Unavailable);
    assert_eq!(
        f.view.facts().map(|facts| facts.name.as_str()),
        Some("Scout"),
        "a failed read keeps what was shown"
    );
    assert!(f.view.conversation().is_some(), "and the conversation");
}

/// **A Bot tab restored at startup reads its Bot again while the window is
/// not yet vouched for**, says it is loading meanwhile — never *Koru AI
/// isn't answering* — and lists its conversations once it has its Bot.
/// Past the bounded while it says the window has no access. The refusal is the real link's: the AI worker's
/// `authorize` answering `forbidden` to a caller with no `bots.manage`
/// (`Failure::unvouched_refusal`, `link.rs`'s `Failure::of`).
#[test]
fn a_restored_tab_reads_its_bot_again_until_the_window_is_vouched_for() {
    let mut f = fixture();
    assert!(f.view.shown_pass(None));
    let seq = f.asked();
    f.answer(seq, f.bot.id, Err(Failure::unvouched_refusal()));
    assert_eq!(
        f.view.load(),
        Load::Loading,
        "Koru AI answered: still loading"
    );
    assert_eq!(f.view.notice(), Some(Words::BotsBotLoading));
    let due = f.view.bot_read_next.expect("asked again on the clock");
    f.view.bot_read_next = Some(due.min(std::time::Instant::now()));
    f.view.shown_pass(None);
    let again = f.asked();
    assert!(again > seq, "without being hidden and shown again");
    f.answer(again, f.bot.id, Ok(f.bot.clone()));
    assert_eq!(f.view.load(), Load::Ready);
    assert_eq!(f.view.notice(), None);
    f.view.shown_pass(None);
    f.listed();

    // Refused for longer than a starting Koru takes: said as no access.
    let mut f = fixture();
    f.view.shown_pass(None);
    let seq = f.asked();
    f.answer(seq, f.bot.id, Err(Failure::unvouched_refusal()));
    f.view
        .vouching
        .age_for_test(crate::bots::link::UNVOUCHED_LIMIT);
    f.view.bot_read_next = Some(std::time::Instant::now());
    f.view.shown_pass(None);
    let seq = f.asked();
    f.answer(seq, f.bot.id, Err(Failure::unvouched_refusal()));
    assert_eq!(f.view.load(), Load::Denied);
    assert_eq!(f.view.notice(), Some(Words::BotsBotNoAccess));
    assert_eq!(f.view.bot_read_next, None, "no more asking");
}

/// **The Bot's refusal words are the Bot adapter's data**, handed to the
/// shared chat when its conversation is bound (the standalone-component
/// rule: the chat crate knows no Bot code). Each lifecycle hold and the
/// settling change reads as the sentence the settings say for it, in every
/// language.
#[test]
fn the_bots_refusal_words_are_handed_to_the_chat_and_match_the_settings() {
    let codes: Vec<&str> = BOT_REFUSALS.iter().map(|(code, _)| *code).collect();
    assert_eq!(
        codes,
        [
            "bot_archived",
            "bot_conversation_trashed",
            "bot_trashed",
            "bot_lifecycle_pending",
            "bot_operation_pending",
            // Added to `BOT_REFUSALS` with the Routine retry refusal
            // (4443d0a0); this list was not, and the test was red since.
            "routine_retry_unsupported",
            // Every Bot is paused (M8): a message to an idle Bot.
            "team_paused"
        ]
    );
    for (code, words) in BOT_REFUSALS {
        assert_eq!(crate::bots::settings::refusal_words(code), *words, "{code}");
        for lang in Lang::ALL {
            assert!(!t(lang, *words).trim().is_empty(), "{code} {lang:?}");
        }
    }
}

#[test]
fn the_chat_retains_loading_notices_but_removes_the_identity_header() {
    let mut f = fixture();
    f.view.shown_pass(None);
    assert_eq!(
        f.view
            .header_lines()
            .iter()
            .map(|line| line.2.as_str())
            .collect::<Vec<_>>(),
        vec![t(f.view.lang, Words::BotsBotLoading)]
    );
    let seq = f.asked();
    f.answer(seq, f.bot.id, Ok(f.bot.clone()));
    assert!(f.view.header_lines().is_empty());
    f.view.with_frame(|_, frame| {
        for key in [
            NAME,
            PAUSED,
            MAIN_MARK,
            SAVES,
            LIMIT,
            region::SETTINGS_BUTTON,
            actions::ACTIONS_BUTTON,
        ] {
            assert!(!frame.contains(key), "removed header control {key:?}");
        }
        assert_eq!(frame.node(HEADER).unwrap().rect.h, 0.0);
        assert!(frame.contains(CONVERSATION));
    });
}

#[test]
fn the_chat_uses_the_full_tab_without_header_or_side_columns() {
    let mut f = listed(&[]);
    for width in [320.0, 628.0, 1000.0] {
        f.view.set_viewport(width, 600.0);
        f.view.with_frame(|_, frame| {
            let chat = f.view.conversation_area(frame);
            assert!(chat.x.abs() < 0.5 && chat.y.abs() < 0.5, "{chat:?}");
            assert!((chat.w - width).abs() < 0.5, "{chat:?}");
            assert!(!frame.contains(list::CONVERSATIONS));
            assert!(!frame.contains(list::CONVERSATIONS_BUTTON));
        });
    }
}

/// **A Bot's conversation offers local path references and nothing else
/// the owner refuses.** The Bot owner delivers a file or folder attached by
/// its path as an ordinary chat's (QUESTIONS O30), so the tab admits that
/// choice, and rename, as an AI chat's (the owner's decision); pin,
/// privacy, the chat's own working folder and its workspace access stay
/// absent. What each offer does to the surface — the
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
            rename: true,
            ..koru_ai_ui::OwnedOffers::default()
        },
        "and nothing else beyond the conversation"
    );
}

#[test]
fn an_archived_bot_retains_its_recovery_notice_without_the_identity_header() {
    let mut f = fixture();
    f.view.shown_pass(None);
    let seq = f.asked();
    let mut archived = f.bot.clone();
    archived.lifecycle = BotLifecycle::Archived;
    f.answer(seq, f.bot.id, Ok(archived));
    assert!(f.view.facts().is_some_and(|facts| facts.archived));
    let keys: Vec<_> = f
        .view
        .header_lines()
        .into_iter()
        .map(|line| line.0)
        .collect();
    assert_eq!(keys, [actions::LIFECYCLE_NOTICE]);
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
        f.publishable("settings open");
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
                Command::ListChats { .. }
                | Command::Read { .. }
                | Command::ReadLibrary { .. }
                | Command::Lifecycle { .. } => {
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
        // Every answer the tests drive leaves a tree AccessKit can take.
        self.publishable("after an owner's answer");
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
        let pressed = View::mouse_down(
            &mut self.view,
            &mut ws,
            rect.x + rect.w * 0.5,
            rect.y + rect.h * 0.5,
            false,
        );
        self.publishable("after a press");
        pressed
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
            6,
            "about, Library, agent and model, access, folder, pause"
        );
        for &card in &cards {
            let rect = frame.nodes[card].rect;
            assert!(rect.x >= region.x - 0.5 && rect.x + rect.w <= region.x + region.w + 0.5);
        }
        for row in [
            "Bot name row",
            "Bot avatar choices",
            "Library standing role",
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

/// **An assistive technology cannot act on the access choice while a save
/// of it is on its way** (the second review's item 11): the choice is drawn
/// held still, and a click on either of its segments is refused before any
/// path acts — as a press would be.
#[test]
fn an_assistive_click_on_an_access_segment_does_nothing_while_its_save_is_on_its_way() {
    let mut f = Fixture::opened(unpaused());
    f.view.perform(Act::Choose(BotAccess::Workspace));
    assert!(f.press(SAVE_ACCESS));
    let _ = f.wrote();
    assert!(
        f.view
            .with_frame(|_, frame| frame.node(ACCESS_CHOICE).is_some_and(|node| node.disabled)),
        "held still while saving"
    );
    for segment in ACCESS_SEGMENTS {
        assert!(
            !f.view.accessibility_action(segment, true),
            "{segment:?}: refused"
        );
    }
    assert_eq!(f.view.settings().access_shown(), BotAccess::Workspace);
    assert!(f.commands.try_recv().is_err(), "nothing else was sent");
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
    assert_eq!(
        roots.len(),
        1,
        "settings is one region, with no duplicate header controls"
    );
    assert_eq!(roots[0].key, SETTINGS);
    assert_eq!(roots[0].label, t(lang, Words::BotsSettings));
    assert_eq!(roots[0].children[0].key, SETTINGS_BUTTON);
    assert_eq!(roots[0].children[0].label, t(lang, Words::Back));
    let cards: Vec<Key> = roots[0]
        .children
        .iter()
        .filter(|c| c.role == ControlRole::Group)
        .map(|c| c.key)
        .collect();
    assert_eq!(
        cards,
        [
            ABOUT_CARD,
            super::library::CARD,
            AGENT_CARD,
            ACCESS_CARD,
            FOLDER_CARD,
            PAUSE_CARD
        ]
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
        // The access card's *May ask other bots* is a switch (M6).
        .filter(|c| {
            matches!(
                c.role,
                ControlRole::Button | ControlRole::TextInput | ControlRole::Toggle
            ) && !c.disabled
        })
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
            Command::ListChats { .. } | Command::ReadLibrary { .. } => {}
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
    let taken = View::key(view, &mut ws, &key, text, ModifiersState::empty());
    // Settings and popovers opened and closed by the keyboard: every step
    // leaves a tree AccessKit can take.
    crate::bots::tests::assert_bot_tab_publishable("after a key", view);
    taken
}

fn named(named: winit::keyboard::NamedKey) -> WinitKey {
    WinitKey::Named(named)
}

#[test]
fn navigator_settings_back_returns_to_the_chat_without_a_hidden_focus_target() {
    let mut f = listed(&[]);
    assert!(!f.view.header_stop());
    assert!(!f.view.focus_header());
    f.view.set_settings_open(true);
    assert_eq!(f.view.focus(), Some(region::SETTINGS_BUTTON));
    assert!(f.press(region::SETTINGS_BUTTON));
    assert!(!f.view.settings_shown());
    assert_eq!(f.view.focus(), None);
    assert!(!f.view.header_stop());
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

#[test]
fn an_avatar_only_save_publishes_the_new_face_to_the_roster() {
    let mut f = Fixture::opened(unpaused());
    assert!(f.view.take_roster_news().is_none());
    assert!(f.view.perform(Act::Avatar(1)));
    assert!(f.press(SAVE_NAME));
    let (request, sent) = f.wrote();
    let BotCommand::Update { config, .. } = sent else {
        panic!("the avatar is saved through Update");
    };
    assert_eq!(config.name, f.bot.config.name);
    assert_eq!(config.avatar, Some(1));
    let mut saved = f.bot.clone();
    saved.config = config;
    saved.revision += 1;
    f.reply(request, Ok(BotResponse::Bot { bot: saved.clone() }));

    assert_eq!(f.view.avatar(), Some(1));
    let news = f
        .view
        .take_roster_news()
        .expect("avatar-only roster update");
    assert_eq!(news, saved);
    assert_eq!(crate::bots::roster::RosterBot::from(news).avatar, 1);
    assert!(f.view.take_roster_news().is_none(), "published once");
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
    assert_eq!(
        tree,
        (false, false, false),
        "navigation belongs to the Bot roster"
    );

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
    // The main conversation is marked in the words its row shows.
    let named = f.view.conversation_name(main);
    let shown = koru_i18n::fill(
        t(Lang::En, Words::BotsMainConversationTitled),
        &["Conversation 0"],
    );
    assert!(named.starts_with(&shown), "{named}");
}

/// **A Bot's conversations are titled as AI chats are, one vocabulary
/// everywhere** (the second window run: a new conversation read as the
/// Bot's name, so two rows read the same; the main one was marked only for
/// a screen reader; the roster called it *Another conversation*; its Trash
/// question read exactly as the Bot's own). Titles as the owner lists
/// them (`list_bot_chats`): the main conversation carries the Bot's name;
/// a new one `koru_ai_core::NEW_CHAT_TITLE`, as the engine now makes it; an
/// older one the Bot's name; one renamed, its own.
#[test]
fn a_bots_conversations_are_titled_as_ai_chats_are() {
    let (new, older, named) = (ChatId::new(), ChatId::new(), ChatId::new());
    let mut f = Fixture::opened(unpaused());
    f.view.set_settings_open(false);
    f.view.shown_pass(None);
    let seq = f.listed();
    let mut page = bot_page(
        &f.bot,
        &[
            (f.bot.main_chat, koru_ai_core::bots::BotChatPurpose::Main),
            (new, koru_ai_core::bots::BotChatPurpose::Additional),
            (older, koru_ai_core::bots::BotChatPurpose::Additional),
            (named, koru_ai_core::bots::BotChatPurpose::Additional),
        ],
    );
    let name = f.bot.config.name.clone();
    page.chats[0].chat.title = name.clone();
    page.chats[1].chat.title = koru_ai_core::NEW_CHAT_TITLE.into();
    page.chats[2].chat.title = name.clone();
    page.chats[3].chat.title = "Builder forums".into();
    f.answers
        .send(Answer::Chats {
            seq,
            bot: f.bot.id,
            result: Ok(page),
        })
        .unwrap();
    f.view.poll();
    f.view.shown_pass(None);
    let lang = f.view.lang;
    // The rows the shared list draws.
    let drawn = |chat: ChatId| {
        f.view
            .conversations
            .surface
            .chats()
            .iter()
            .find(|row| row.id == chat)
            .map(|row| koru_chats_ui::text::title(lang, row).to_owned())
            .expect("listed")
    };
    assert_eq!(
        drawn(f.bot.main_chat),
        t(lang, Words::BotsMainConversation),
        "the main one, marked where a person sees it"
    );
    assert_eq!(
        drawn(new),
        t(lang, Words::ChatsNew),
        "new, in the reader's words"
    );
    assert_eq!(
        drawn(older),
        t(lang, Words::ChatsNew),
        "never the Bot's name twice"
    );
    assert_eq!(drawn(named), "Builder forums");
    // The Trash question names it as a conversation of its Bot.
    let (title, _, _) = f
        .view
        .conversation_trash_words(new, crate::bots::stops::WorkNow::Unknown)
        .expect("asked");
    let (bot_title, _, _) = f
        .view
        .confirm_words(
            crate::bots::lifecycle::Ask::Trash,
            crate::bots::stops::WorkNow::Unknown,
        )
        .expect("asked");
    assert_eq!(
        title,
        koru_i18n::fill(
            t(lang, Words::BotsTrashConversationTitle),
            &[t(lang, Words::ChatsNew), name.as_str()]
        )
    );
    assert_ne!(title, bot_title, "never the Bot's own question");
    // The roster's activity row says the same words.
    let titles: std::collections::HashMap<ChatId, String> =
        f.view.conversation_titles().into_iter().collect();
    assert_eq!(titles[&named], "Builder forums");
    assert_eq!(titles[&new], t(lang, Words::ChatsNew));
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
            refusals: BOT_REFUSALS,
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
            result: Err(Failure::lost()),
        })
        .unwrap();
    f.view.poll();
    assert_eq!(f.view.conversations.listed().len(), 2, "kept");
    assert_eq!(
        f.view.conversations.notice(),
        Some(Words::BotsConversationsUnavailable)
    );
}

// ------------------------------- a change still being delivered (P1.8)

/// Every published control, depth first.
fn every_control(controls: Vec<Control>) -> Vec<Control> {
    let mut out = Vec::new();
    for mut control in controls {
        let children = std::mem::take(&mut control.children);
        out.push(control);
        out.extend(every_control(children));
    }
    out
}

/// The published words of the region's control `key`, if it is published.
fn published(view: &BotView, key: Key) -> Option<String> {
    every_control(view.controls())
        .into_iter()
        .find(|control| control.key == key)
        .map(|control| control.label)
}

/// **A Pause the owner renewed is said as still being delivered — never as
/// not applied — holds the commands the owner would refuse inert, and goes
/// by itself.** The fakes answer as the engine does after a Pause its
/// execution owner refused as conflicting: `read_operation` of the person's
/// own request answers `rejected` (`settle_refused_bot_pause`,
/// `crates/koru-ai-sqlite/src/repository/bots/admission.rs`), and the Bot's
/// `read` says paused with `pending_change` naming the AI owner's renewal
/// (`bot_pending_change`, `apps/koru-ai-server/src/worker/bots/admission.rs`).
/// While the read names it, the Bot is read again on the bounded clock
/// while shown; once it names nothing, the sentence and the inert state go.
#[test]
fn a_renewed_pause_is_said_as_still_being_delivered_and_goes_by_itself() {
    let mut f = Fixture::opened(unpaused());
    assert!(f.press(PAUSE));
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
    f.reply(
        pause,
        Ok(pending(
            pause,
            &bot,
            BotOperationKind::Pause,
            BotOperationStatus::Rejected,
        )),
    );
    assert!(
        f.keys().contains(&PAUSE_PENDING),
        "on its way until the read says"
    );
    assert!(
        !f.keys().contains(&SETTINGS_NOTICE),
        "never *not applied* on its own"
    );
    // The read the rejection left owed.
    f.view.shown_pass(None);
    let seq = f.asked();
    let mut renewed = bot.clone();
    renewed.paused = true;
    renewed.revision += 2;
    renewed.pending_change = Some(koru_ai_core::bots::BotPendingChange {
        request_id: BotRequestId::mint(),
        kind: BotOperationKind::Pause,
        since_ms: 1,
        last_answer: Some("bot_conflict".into()),
    });
    f.answer(seq, bot.id, Ok(renewed.clone()));
    assert!(!f.keys().contains(&SETTINGS_NOTICE), "nothing refused");
    assert_eq!(
        published(&f.view, SETTINGS_DELIVERING).as_deref(),
        Some(t(f.view.lang, Words::BotsDeliveringPause)),
        "which change is still on its way"
    );
    let inert = |f: &Fixture, key| {
        f.view
            .with_frame(|_, frame| frame.node(key).is_some_and(|node| node.disabled))
    };
    assert!(inert(&f, PAUSE), "*Resume bot* inert, not hidden");
    assert!(!f.view.settings().can_toggle_pause());
    f.press(PAUSE);
    assert!(
        !f.commands
            .try_iter()
            .any(|command| matches!(command, Command::Write { .. })),
        "a press sends nothing"
    );
    // Read again on the bounded clock while shown.
    let due = f
        .view
        .bot_read_next
        .expect("read again while it is delivered");
    f.view.bot_read_next = Some(due.min(std::time::Instant::now()));
    f.view.shown_pass(None);
    let seq = f.asked();
    renewed.pending_change = None;
    f.answer(seq, bot.id, Ok(renewed));
    assert_eq!(published(&f.view, SETTINGS_DELIVERING), None, "delivered");
    assert!(!inert(&f, PAUSE), "offered again");
    assert_eq!(f.view.bot_read_next, None, "and no more reads of its own");
}

/// **An Archive refused because a change is still being delivered says
/// which change**, once the Bot's read names it (`bot_operation_pending`,
/// `BotError::Settling`): the specific sentence in the tab's lifecycle
/// block, never only *an earlier change*.
#[test]
fn an_archive_refused_while_a_resume_is_delivered_says_so() {
    let mut bot = unpaused();
    bot.pending_change = Some(koru_ai_core::bots::BotPendingChange {
        request_id: BotRequestId::mint(),
        kind: BotOperationKind::Resume,
        since_ms: 1,
        last_answer: None,
    });
    let mut f = Fixture::opened(bot);
    assert!(f.view.begin_lifecycle(crate::bots::lifecycle::Ask::Archive));
    let (archive, _) = f.wrote();
    f.reply(
        archive,
        Err(Failure::Refused(
            crate::bots::settings::BOT_OPERATION_PENDING.into(),
        )),
    );
    let said = f.view.lifecycle_said().expect("the refusal is said");
    assert_eq!(said.words, Words::BotsDeliveringResume);
    assert!(said.refused);
}

/// **The mode chip as it was drawn** (P1.8 §20): the properties of the
/// shared button painted inside the chip's element, read from the
/// inspector over the tab's own frame — what reached the scene, not what
/// the tab meant to draw.
fn mode_chip_drawn(view: &BotView) -> std::collections::HashMap<String, String> {
    let snapshot = view.with_frame(|tree, frame| {
        let capture = koru_draw::inspect::Frame::begin(
            1,
            vello::kurbo::Affine::IDENTITY,
            vello::kurbo::Rect::new(0.0, 0.0, 4000.0, 4000.0),
        );
        let mut scene = vello::Scene::new();
        crate::input_engine::with(|engine| {
            frame.paint(tree, &mut scene, vello::kurbo::Affine::IDENTITY, engine)
        });
        capture.finish()
    });
    let chip = snapshot
        .records
        .iter()
        .position(|record| record.name == "Bot mode chip")
        .expect("the mode chip is drawn");
    let button = snapshot
        .records
        .iter()
        .find(|record| record.parent == Some(chip) && record.name == "KoruButton")
        .expect("drawn by the shared button");
    button
        .properties
        .iter()
        .map(|property| (property.name.to_owned(), property.value.clone()))
        .collect()
}

/// The ink role a drawn property names: `Ink::Notice` of
/// `Ink::Notice -> #rrggbb`.
fn ink_of(drawn: &std::collections::HashMap<String, String>, name: &str) -> String {
    drawn
        .get(name)
        .unwrap_or_else(|| panic!("no {name} drawn: {drawn:?}"))
        .split(" -> ")
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// The lifecycle half of the tab (S14).
mod agent;
mod create;
mod library;
mod lifecycle;
mod mentions;
mod onboard;

// ------------------------------------------- the first window run, finding 8

/// The published node of the Bot tab whose locator ends in `#part`.
fn node_of(view: &BotView, part: &str) -> Option<koru_workspace_host::accessibility::SemanticNode> {
    fn find(
        node: &koru_workspace_host::accessibility::SemanticNode,
        part: &str,
    ) -> Option<koru_workspace_host::accessibility::SemanticNode> {
        if node.occurrence.part.name() == part {
            return Some(node.clone());
        }
        node.children.iter().find_map(|child| find(child, part))
    }
    find(&crate::bots::tests::bot_tab_tree(view), part)
}

/// **Every control is published with what a client can do to it, and
/// its state as the platform reads it** (the first window run: *Settings*
/// with no Invoke, the access choices with no pattern and no state). A
/// button is pressable and says no selection — a selection takes its
/// Invoke away (`accesskit_consumer::Node::is_invocable`); a radio button
/// says checked, which is what `accesskit_windows` reads for its
/// SelectionItem; a field takes a client's text.
#[test]
fn controls_are_published_with_their_actions_and_states() {
    use accesskit::{Action, Role, Toggled};
    let f = Fixture::opened(unpaused());
    let settings = node_of(&f.view, region::SETTINGS_BUTTON.part).expect("Settings");
    assert_eq!(settings.role, Role::Button);
    assert!(settings.actions.contains(&Action::Click), "pressable");
    assert_eq!(settings.selected, None, "no selection on a button");
    let segments: Vec<_> = region::ACCESS_SEGMENTS
        .iter()
        .map(|key| node_of(&f.view, key.part).expect("a segment"))
        .collect();
    for segment in &segments {
        assert_eq!(segment.role, Role::RadioButton);
        assert!(segment.actions.contains(&Action::Click));
        assert_eq!(segment.selected, None);
    }
    assert_eq!(segments[0].toggled, Some(Toggled::True), "the chosen one");
    assert_eq!(segments[1].toggled, Some(Toggled::False));
    let name = node_of(&f.view, region::NAME_FIELD.part).expect("the name");
    assert!(name.actions.contains(&Action::SetValue));
    // The two lines drawn and once not published.
    for (part, words) in [
        (region::SELECTED_PAGES_TITLE.part, Words::BotsSelectedPages),
        (region::RUNS_HERE.part, Words::BotsRunsOnThisComputer),
    ] {
        let line = node_of(&f.view, part).unwrap_or_else(|| panic!("{part} published"));
        assert_eq!(line.role, Role::Label);
        assert_eq!(line.label.as_deref(), Some(t(Lang::En, words)));
    }
}

/// **A client's text lands in the field as typing it would**: the name
/// field shows it and the draft holds it (it was published writable and
/// ignored a set-value). Nothing for a key that is no field.
#[test]
fn a_clients_text_sets_the_name_as_typing_would() {
    let mut f = Fixture::opened(unpaused());
    assert!(
        f.view
            .accessibility_set_value(region::NAME_FIELD, "Scout Two")
    );
    assert_eq!(f.view.settings().name_shown(), "Scout Two");
    assert_eq!(
        published(&f.view, region::NAME_FIELD).map(|_| ()),
        Some(()),
        "still published"
    );
    assert_eq!(
        every_control(f.view.controls())
            .into_iter()
            .find(|control| control.key == region::NAME_FIELD)
            .and_then(|control| control.value),
        Some("Scout Two".to_owned())
    );
    assert!(!f.view.accessibility_set_value(region::SETTINGS_BUTTON, "x"));
}

/// **A scroll of the settings republishes their bounds**: the tree's
/// fingerprint moves with every rect, so a client is never left holding
/// where a control was before the wheel.
#[test]
fn a_scroll_of_the_settings_moves_the_published_fingerprint() {
    let mut f = Fixture::opened(unpaused());
    f.view.set_viewport(800.0, 420.0);
    let fingerprint = |view: &BotView| {
        let mut rev = koru_workspace_host::accessibility::Revision::new();
        view.revise(&mut rev);
        rev.finish()
    };
    let before = fingerprint(&f.view);
    let name_before = node_of(&f.view, region::NAME_FIELD.part)
        .and_then(|node| node.bounds)
        .expect("placed");
    f.view.scroll_region(120.0);
    assert_ne!(fingerprint(&f.view), before, "republished after the wheel");
    let name_after = node_of(&f.view, region::NAME_FIELD.part)
        .and_then(|node| node.bounds)
        .expect("placed");
    assert!(
        name_after.y < name_before.y,
        "{name_before:?} → {name_after:?}"
    );
}

/// **While the settings cover the conversation, its composer takes no
/// keys** — entered as the window enters them (`View::key`), with nothing
/// of the settings focused, as after a press on the content's background
/// (the second window run: *hello fixture* typed under the settings went
/// to the hidden composer, and the next message was sent doubled). Closed,
/// the conversation answers again.
#[test]
fn keys_never_reach_the_composer_under_the_settings() {
    let mut f = listed(&[]);
    f.view.set_settings_open(true);
    f.view.focus = None;
    for typed in ["h", "i"] {
        assert!(
            !key(&mut f.view, WinitKey::Character(typed.into()), Some(typed)),
            "{typed}: not the hidden conversation's"
        );
    }
    assert!(
        !key(&mut f.view, named(winit::keyboard::NamedKey::Enter), None),
        "Enter sends nothing"
    );
    f.view.set_settings_open(false);
    assert!(
        key(&mut f.view, WinitKey::Character("h".into()), Some("h")),
        "the conversation, shown, takes typing again"
    );
}
