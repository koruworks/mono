// SPDX-License-Identifier: MPL-2.0

//! The first-bot surfaces in the window: when the empty view is shown, what
//! the dialog owns while it stands, where the keyboard goes back, what a
//! finished creation opens, and the one clock the faces share.

use std::time::{Duration, Instant};

use accesskit::Role;
use koru_ai_core::bots::{
    BotActivity, BotActivityState, BotCommand, BotCreation, BotCreationPhase, BotCreationStage,
    BotId, BotResponse,
};
use koru_draw::face::FacePose;
use koru_store_core::ids::WorkspaceId;
use koru_workspace_host::View;
use koru_workspace_host::accessibility::{Scope, SemanticNode, ViewSlot, WindowSlot};
use koru_workspace_host::routing::{Blockers, ModalKind};
use winit::keyboard::{Key, NamedKey};

use super::*;
use crate::bots::BotView;
use crate::bots::link::{Answer, BotsLink, Command};
use crate::bots::roster::RosterBot;
use crate::bots::view::new_bot::NewBotDialog;
use crate::mainview::harness::*;

const WINDOW: WindowSlot = WindowSlot::from_raw(1);
const VIEW: ViewSlot = ViewSlot::from_raw(1);

/// The window's workspace, as the Bot service names it.
fn home(v: &MainView) -> WorkspaceId {
    WorkspaceId::from_uuid(v.current_workspace().expect("a workspace").as_uuid())
}

/// A window on a document, its sidebar open on the Bots panel, whose
/// roster has not answered yet.
fn on_bots() -> (MainView, koru_workspace::Workspace) {
    let (mut v, w) = on_document();
    v.seat_accessibility(WINDOW);
    if !v.sidebar.is_open() {
        assert!(v.toggle_sidebar(), "the column would not open");
    }
    v.explore.show_bots();
    let home = home(&v);
    v.explore.bots_mut().set_workspace(Some(home));
    (v, w)
}

fn listed(v: &mut MainView, bots: Vec<RosterBot>) {
    v.explore.bots_mut().listed_for_test(bots);
}

fn a_bot(v: &MainView, name: &str) -> koru_ai_core::bots::Bot {
    crate::bots::tests::bot(BotId::mint(), home(v), name, false)
}

fn named(key: NamedKey) -> Key {
    Key::Named(key)
}

fn press(
    v: &mut MainView,
    w: &mut koru_workspace::Workspace,
    key: Key,
    text: Option<&str>,
) -> bool {
    View::key(v, &mut w.ws, &key, text, NONE)
}

fn all_nodes(v: &MainView) -> Vec<SemanticNode> {
    fn flatten(node: &SemanticNode, out: &mut Vec<SemanticNode>) {
        out.push(node.clone());
        for child in &node.children {
            flatten(child, out);
        }
    }
    let roots = v.semantics(VIEW, Scope::Whole);
    crate::bots::tests::assert_publishable("the window", &roots);
    let mut out = Vec::new();
    for node in &roots {
        flatten(node, &mut out);
    }
    out
}

fn locators(v: &MainView) -> Vec<String> {
    all_nodes(v)
        .iter()
        .map(|node| node.occurrence.locator())
        .collect()
}

/// **The empty view is shown when the workspace has no Bot, and only
/// then.** Not before the roster has answered (a roster still loading is
/// never *no Bots*), not with one Bot, not while another navigator view is
/// showing, not with the sidebar shut, and not over a Bot tab.
#[test]
fn the_empty_view_appears_with_no_bots_and_not_with_one() {
    let (mut v, _w) = on_bots();
    assert!(
        !v.bots_empty_visible(),
        "the roster has not answered: not known to be empty"
    );
    listed(&mut v, Vec::new());
    assert!(v.bots_empty_visible(), "answered with none");
    let published = locators(&v);
    assert!(
        published.iter().any(|l| l.contains("s:bots-empty#new-bot")),
        "its button is published: {published:?}"
    );
    assert!(published.iter().any(|l| l.contains("s:bots-empty#title")));

    let scout = RosterBot::from(a_bot(&v, "Scout"));
    listed(&mut v, vec![scout.clone()]);
    assert!(!v.bots_empty_visible(), "one Bot: the tabs are back");
    assert!(
        !locators(&v).iter().any(|l| l.contains("s:bots-empty")),
        "and nothing of the view is published"
    );

    listed(&mut v, Vec::new());
    assert!(v.bots_empty_visible());
    v.explore.show_chats();
    assert!(!v.bots_empty_visible(), "another navigator view");
    v.explore.show_bots();
    assert!(v.bots_empty_visible());
    assert!(v.toggle_sidebar());
    assert!(!v.bots_empty_visible(), "the panel is shut");
    assert!(v.toggle_sidebar());
    assert!(v.bots_empty_visible());
    // A Bot tab in front — the create form — is never covered.
    assert!(v.open_bot_create_tab());
    assert!(!v.bots_empty_visible(), "the create form shows");
}

/// **The tab the view covers gets no key and no press.** A character that
/// would have been typed into the document goes nowhere; the same
/// character lands in the document once the view is gone.
#[test]
fn the_covered_tab_takes_no_key_while_the_view_is_shown() {
    let (mut v, mut w) = on_bots();
    listed(&mut v, Vec::new());
    assert!(v.bots_empty_visible());
    assert!(!press(&mut v, &mut w, ch("x"), Some("x")), "not the tab's");
    assert_eq!(v.bots_empty_key(&ch("x"), NONE), Some(false));
    // A press in the content area is the view's.
    let c = v.content();
    assert!(v.bots_empty_mouse_down(c.x + 5.0, c.y + 5.0));
    assert!(!v.new_bot_open(), "not on the button: nothing opened");
    // And a right press there opens no menu of the tab nobody can see.
    assert!(View::context_menu(&mut v, &mut w.ws, c.x + 5.0, c.y + 5.0));
    assert!(v.menu.is_none());
    assert!(v.bots_empty_covers(c.x + 5.0, c.y + 5.0));
    assert!(
        !v.bots_empty_covers(1.0, c.y + 5.0),
        "the panel is not covered"
    );

    let scout = RosterBot::from(a_bot(&v, "Scout"));
    listed(&mut v, vec![scout]);
    assert_eq!(v.bots_empty_key(&ch("x"), NONE), None, "the view is gone");
    assert!(!v.bots_empty_mouse_down(c.x + 5.0, c.y + 5.0));
    assert!(
        press(&mut v, &mut w, ch("x"), Some("x")),
        "the document's again"
    );
}

/// **Opened from the empty view by the keyboard, the dialog owns every key
/// while it stands, and Escape gives the keyboard back to the button that
/// opened it, with nothing created.**
#[test]
fn the_dialog_owns_the_keys_and_escape_returns_the_keyboard() {
    let (mut v, mut w) = on_bots();
    listed(&mut v, Vec::new());
    let tabs = v.tabs().len();
    assert!(press(&mut v, &mut w, named(NamedKey::Tab), None));
    let button = v.accessibility_focus(VIEW).expect("a focus").locator();
    assert!(button.contains("s:bots-empty#new-bot"), "{button}");
    assert!(press(&mut v, &mut w, named(NamedKey::Enter), None));
    assert!(v.new_bot_open(), "Enter on New bot opens the dialog");
    assert_eq!(v.blockers(), Blockers::Modal(ModalKind::NewBot));
    assert_eq!(v.tabs().len(), tabs, "a dialog, not a tab");

    // The dialog alone is published, as a dialog, and its name field has
    // the keyboard.
    let nodes = all_nodes(&v);
    let dialog = nodes
        .iter()
        .find(|node| node.role == Role::Dialog)
        .expect("a dialog");
    assert!(dialog.modal);
    assert_eq!(
        dialog.label.as_deref(),
        Some(koru_i18n::t(
            koru_workspace_host::accessibility::ui_lang(),
            Words::BotsNewBot
        ))
    );
    assert!(
        !nodes
            .iter()
            .any(|node| node.occurrence.locator().contains("s:bots-empty")),
        "nothing under the dialog is published"
    );
    let focus = v.accessibility_focus(VIEW).expect("a focus").locator();
    assert!(focus.contains("s:new-bot#name"), "{focus}");
    assert!(
        nodes.iter().any(|node| node.occurrence.locator() == focus),
        "the focused control is in the tree"
    );

    // Typing goes to the dialog's name, never to the document under it.
    for letter in ["S", "c"] {
        assert!(press(&mut v, &mut w, ch(letter), Some(letter)));
    }
    assert_eq!(v.first_bot.dialog.as_ref().unwrap().name(), "Sc");
    let field = all_nodes(&v)
        .into_iter()
        .find(|node| node.role == Role::TextInput)
        .expect("the name field");
    assert_eq!(field.value.as_deref(), Some("Sc"));
    // A chord is the dialog's too: nothing under the scrim acts.
    assert!(press(&mut v, &mut w, named(NamedKey::F6), None));
    assert!(v.new_bot_open());

    assert!(press(&mut v, &mut w, named(NamedKey::Escape), None));
    assert!(!v.new_bot_open(), "Escape closes it");
    assert_eq!(v.tabs().len(), tabs, "and nothing was made");
    assert!(v.bots_empty_visible());
    let back = v.accessibility_focus(VIEW).expect("a focus").locator();
    assert_eq!(back, button, "the keyboard is back on the button");
    assert!(locators(&v).contains(&back));
}

/// **The heading's `+` opens the same dialog**, never a second one and
/// never the create tab; Cancel closes it and nothing was made. The full
/// form is still there: *Open the full form* puts the dialog's draft in the
/// strip as the create tab.
#[test]
fn the_heading_opens_the_dialog_and_the_full_form_is_reachable() {
    let (mut v, w) = on_bots();
    let scout = RosterBot::from(a_bot(&v, "Scout"));
    listed(&mut v, vec![scout]);
    let tabs = v.tabs().len();
    assert!(v.apply(&w.ws, crate::panes::Outcome::CreateBot));
    assert!(v.new_bot_open());
    assert!(v.apply(&w.ws, crate::panes::Outcome::CreateBot), "again");
    assert_eq!(v.tabs().len(), tabs, "one dialog, and no tab");
    // The roster's own ask opens it too.
    v.first_bot.dialog = None;
    assert!(v.explore.bots_mut().ask_create());
    v.maintain_bots();
    assert!(v.new_bot_open());
    assert_eq!(v.tabs().len(), tabs);

    // Cancel: the pointer on the dialog's Cancel.
    let cancel = v
        .first_bot
        .dialog
        .as_ref()
        .unwrap()
        .with_frame(|frame, _| frame.node(crate::bots::view::new_bot::CANCEL).unwrap().rect);
    assert!(v.new_bot_mouse_down(cancel.x + cancel.w * 0.5, cancel.y + cancel.h * 0.5));
    assert!(!v.new_bot_open());
    assert_eq!(v.tabs().len(), tabs, "nothing made");
    assert!(!v.first_bot.empty.focused(), "not the empty view's to take");

    // A press outside the panel dismisses nothing.
    assert!(v.open_new_bot_dialog(Opener::Elsewhere));
    assert!(v.new_bot_mouse_down(2.0, 2.0));
    assert!(
        v.new_bot_open(),
        "a name somebody typed is not lost to a stray click"
    );

    // The full form, on the same draft.
    let draft = v.first_bot.dialog.as_ref().unwrap().draft_bot();
    assert!(
        v.first_bot
            .dialog
            .as_mut()
            .unwrap()
            .accessibility_set_name("Scout")
    );
    let outcome = v
        .first_bot
        .dialog
        .as_mut()
        .unwrap()
        .activate(crate::bots::view::new_bot::FULL_FORM);
    assert!(v.settle_new_bot(outcome));
    assert!(!v.new_bot_open());
    assert_eq!(v.tabs().len(), tabs + 1, "the create tab");
    let TabView::Bot(view) = &v.tabs().active_tab().view else {
        panic!("a Bot tab in front");
    };
    assert!(view.is_creating());
    assert_eq!(view.create_bot(), draft, "the same draft");
    assert_eq!(v.tabs().active_tab().label, "Scout", "its name came along");
}

/// Runtime configuration opens Settings without abandoning the new Bot draft.
#[test]
fn new_bot_runtime_setup_opens_agent_providers_and_keeps_the_draft_tab() {
    let (mut v, _) = on_bots();
    let tabs = v.tabs().len();
    assert!(v.open_new_bot_dialog(Opener::Elsewhere));
    let dialog = v.first_bot.dialog.as_mut().unwrap();
    dialog.accessibility_set_name("Grok bot");
    let draft = dialog.draft_bot();
    let outcome = dialog.activate(crate::bots::view::new_bot::MANAGE_AGENTS);
    assert_eq!(outcome, Outcome::ManageAgents);
    assert!(v.settle_new_bot(outcome));
    assert!(!v.new_bot_open());
    assert_eq!(v.tabs().len(), tabs + 2);
    assert!(
        matches!(&v.tabs().active_tab().view, TabView::Settings(settings)
        if settings.page() == koru_settings_ui::PageId::AgentProviders)
    );
    assert!(
        v.tabs()
            .iter()
            .any(|tab| matches!(&tab.view, TabView::Bot(bot)
        if bot.create_bot() == draft && tab.label == "Grok bot"))
    );
}

/// **On success the dialog closes and the new Bot's chat opens**: the
/// owner's answer that the Bot is made turns the dialog's host into that
/// Bot's tab, which the window puts in front. The Create went out once,
/// through the create form's command.
#[test]
fn a_finished_creation_closes_the_dialog_and_opens_the_bots_chat() {
    let (mut v, mut w) = on_bots();
    listed(&mut v, Vec::new());
    let home = home(&v);
    let (link, commands, answers) = BotsLink::test_pair();
    let mut host = BotView::creating_with_link(home, link);
    // An agent for the draft: the catalog is the dialog's own tests'.
    host.create_form_mut()
        .unwrap()
        .edit_model(crate::bots::settings::ModelDraft {
            model: koru_ai_core::ModelSelection {
                provider: koru_ai_core::AiProviderId::parse("agent-codex").unwrap(),
                requested_model: "default".into(),
                default_reasoning: koru_ai_core::ReasoningChoice::Default,
            },
            mode: None,
            fast: None,
        });
    v.first_bot.dialog = Some(Box::new(NewBotDialog::over(host, None, v.viewport)));
    let tabs = v.tabs().len();
    for letter in ["S", "c", "o", "u", "t"] {
        assert!(press(&mut v, &mut w, ch(letter), Some(letter)));
    }
    assert!(
        press(&mut v, &mut w, named(NamedKey::Enter), None),
        "Enter creates"
    );
    let (bot, request, command) = match commands.try_recv().expect("a command went") {
        Command::Write {
            bot,
            request,
            command,
        } => (bot, request, *command),
        _ => panic!("a creation command"),
    };
    let BotCommand::CreateBotWithRole { config, .. } = &command else {
        panic!("the create form's command: {command:?}");
    };
    assert_eq!(config.name, "Scout");
    assert_eq!(config.avatar, Some(0), "the face shown is the face sent");
    assert!(commands.try_recv().is_err(), "once");
    assert!(v.new_bot_open(), "up until the owner answers");
    assert!(v.new_bot_deadline().is_some(), "and waiting for it");

    // The Bot as the owner made it: with the face the request named.
    let mut made = crate::bots::tests::bot(bot, home, "Scout", false);
    made.config.avatar = config.avatar;
    let main_chat = made.main_chat;
    answers
        .send(Answer::Wrote {
            bot,
            request,
            result: Ok(BotResponse::Creation {
                creation: BotCreation {
                    request_id: request,
                    bot_id: bot,
                    home_workspace: home,
                    name: "Scout".into(),
                    stage: BotCreationStage::Completed,
                    phase: BotCreationPhase::RolePending,
                    reason: None,
                    retryable: false,
                    role_page: None,
                    role_page_unconfirmed: false,
                    folder_binding: None,
                    left_behind: None,
                    bot: Some(made),
                },
            }),
        })
        .expect("the dialog's end is open");
    assert!(v.maintain_new_bot());
    assert!(!v.new_bot_open(), "the dialog closed");
    assert_eq!(v.new_bot_deadline(), None);
    assert_eq!(v.tabs().len(), tabs + 1);
    let tab = v.tabs().active_tab();
    assert_eq!(tab.view.chat_id(), Some(main_chat), "its chat is in front");
    assert_eq!(tab.label, "Scout");
    let TabView::Bot(view) = &tab.view else {
        panic!("a Bot tab");
    };
    assert_eq!(view.bot(), Some(bot));
    assert_eq!(view.avatar(), Some(0), "wearing the face it was made with");
    assert!(!v.bots_empty_visible(), "a Bot tab is in front");
}

/// **One clock, one deadline, and none while nothing works.** With no Bot
/// working the window asks for no frame at all; with two working there is
/// one deadline, the start of the clock's next step, whichever face asks;
/// a Bot waiting on the person is not working; reduced motion and a hidden
/// roster both end the deadline.
#[test]
fn the_faces_share_one_deadline_and_ask_for_none_while_nothing_works() {
    let (mut v, _w) = on_bots();
    let (a, b, c) = (a_bot(&v, "Ada"), a_bot(&v, "Bo"), a_bot(&v, "Cy"));
    let rows: Vec<RosterBot> = [&a, &b, &c]
        .into_iter()
        .cloned()
        .map(RosterBot::from)
        .collect();
    listed(&mut v, rows.clone());
    let now = Instant::now();

    assert!(!v.sync_bot_faces(now), "nothing works: nothing to repaint");
    assert!(!v.bot_faces().animating());
    assert_eq!(v.bot_faces_deadline(), None, "and no deadline at all");
    assert_eq!(v.bot_faces().deadline(now), None);

    // Ada and Bo have a reply running; Cy is waiting on the person.
    let activity = |bot: &koru_ai_core::bots::Bot, state| BotActivity {
        bot_id: bot.id,
        chat_id: bot.main_chat,
        state,
    };
    v.explore.bots_mut().activity_read_for_test(
        vec![
            activity(&a, BotActivityState::Running),
            activity(&b, BotActivityState::Running),
            activity(&c, BotActivityState::NeedsYou),
        ],
        false,
        now,
    );
    assert!(v.sync_bot_faces(now), "work began: repaint");
    let clock = v.bot_faces().clone();
    assert!(clock.animating());
    let due = clock.deadline(now).expect("a deadline while faces move");
    assert_eq!(
        due,
        now + Duration::from_millis(koru_draw::face::FACE_FRAME_MS),
        "the start of the next step"
    );
    assert_eq!(
        clock.deadline(now + Duration::from_millis(20)),
        Some(due),
        "one deadline for the step, whenever it is asked"
    );
    assert!(
        v.bot_faces_deadline().is_some(),
        "and the window's is that one"
    );
    // Both working faces move, each from the same clock; the others rest.
    let later = now + Duration::from_secs(1);
    for row in &rows[..2] {
        assert_ne!(
            clock.pose(row.avatar, true, later),
            FacePose::STILL,
            "{}",
            row.name
        );
    }
    assert_eq!(clock.pose(rows[2].avatar, false, later), FacePose::STILL);
    // A pass at the deadline owes a repaint; one before it does not.
    assert!(!v.sync_bot_faces(now + Duration::from_millis(10)));
    assert!(v.sync_bot_faces(due));

    // A Bot tab in front wears the same clock's pose: moving for Ada.
    assert!(v.open_bot_tab(rows[0].clone()));
    v.sync_bot_faces(later);
    let pose_of = |v: &MainView| match &v.tabs().active_tab().view {
        TabView::Bot(view) => view.face_pose(),
        _ => panic!("a Bot tab in front"),
    };
    assert_eq!(pose_of(&v), v.bot_faces().pose(rows[0].avatar, true, later));
    assert_ne!(pose_of(&v), FacePose::STILL);
    // Cy's tab: waiting on the person is not working.
    assert!(v.open_bot_tab(rows[2].clone()));
    v.sync_bot_faces(later);
    assert_eq!(pose_of(&v), FacePose::STILL);

    // Reduced motion: every face rests and nothing is asked for.
    v.reduced_motion = true;
    assert!(v.sync_bot_faces(later));
    assert_eq!(v.bot_faces_deadline(), None);
    assert_eq!(
        v.bot_faces().pose(rows[0].avatar, true, later),
        FacePose::STILL
    );
    v.reduced_motion = false;
    v.sync_bot_faces(later);
    assert!(v.bot_faces_deadline().is_some());

    // The roster hidden and no working Bot's tab in front: no deadline.
    v.explore.show_chats();
    v.sync_bot_faces(later);
    assert_eq!(
        v.bot_faces_deadline(),
        None,
        "no face of a working Bot is on screen"
    );
}

/// **The dialog's picked face moves by the window's one clock, and the
/// clock stops with the dialog.** With nothing working and no dialog the
/// window asks for no frame. Opening the dialog starts the clock, which is
/// the one deadline (the start of its next step); the dialog's picked face
/// takes that clock's pose and no other tile moves. Reduced motion ends the
/// deadline and stills the face. Closing the dialog ends the deadline: no
/// wake-up is asked for once it is gone.
#[test]
fn the_dialogs_picked_face_moves_by_the_windows_clock_and_stops_with_it() {
    let (mut v, mut w) = on_bots();
    listed(&mut v, Vec::new());
    assert!(!v.sync_bot_faces(Instant::now()));
    assert_eq!(v.bot_faces_deadline(), None, "nothing moves yet");

    assert!(v.open_new_bot_dialog(Opener::Elsewhere));
    assert!(v.new_bot_open());
    assert!(v.bot_faces().animating(), "the clock runs from the open");
    let now = Instant::now();
    let due = v.bot_faces().deadline(now).expect("a deadline");
    assert!(
        due > now && due <= now + Duration::from_millis(koru_draw::face::FACE_FRAME_MS),
        "the start of the clock's next step"
    );
    assert!(v.bot_faces_deadline().is_some());

    let later = Instant::now() + Duration::from_secs(1);
    assert!(v.sync_bot_faces(later), "a step passed: repaint");
    let dialog = |v: &MainView| {
        v.first_bot
            .dialog
            .as_ref()
            .expect("the dialog")
            .poses_at(later)
    };
    let poses = dialog(&v);
    assert_eq!(
        poses[0],
        v.bot_faces().pose(0, true, later),
        "the picked face, in the window clock's pose"
    );
    assert_ne!(poses[0], FacePose::STILL);
    assert!(
        poses[1..].iter().all(|pose| *pose == FacePose::STILL),
        "every other face rests"
    );
    // One wake-up per step, however often the window is asked.
    assert!(
        !v.sync_bot_faces(later),
        "the same step: nothing to repaint"
    );
    assert_eq!(
        v.bot_faces().deadline(later),
        v.bot_faces().deadline(later + Duration::from_millis(1))
    );

    // Reduced motion: no deadline, and the picked face rests.
    v.reduced_motion = true;
    assert!(v.sync_bot_faces(later));
    assert_eq!(v.bot_faces_deadline(), None);
    assert!(dialog(&v).iter().all(|pose| *pose == FacePose::STILL));
    v.reduced_motion = false;
    assert!(v.sync_bot_faces(later));
    assert!(v.bot_faces_deadline().is_some());
    assert_ne!(dialog(&v)[0], FacePose::STILL);

    // Closed: the clock stops, and nothing is asked for.
    assert!(press(&mut v, &mut w, named(NamedKey::Escape), None));
    assert!(!v.new_bot_open());
    assert!(v.sync_bot_faces(later), "the clock stopped: repaint");
    assert!(!v.bot_faces().animating());
    assert_eq!(v.bot_faces_deadline(), None, "no wake-up once it is closed");
    assert!(!v.sync_bot_faces(later + Duration::from_secs(1)));
}
