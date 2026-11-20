// SPDX-License-Identifier: MPL-2.0

//! The quick setup over a Bot's empty main conversation (QUESTIONS O155):
//! when it shows, that it is no part of the conversation, what its two
//! steps write, how it ends, and what a keyboard and a screen reader get.

use koru_ai_core::bots::library::{BotKnowledgeReceipt, BotLibraryView};
use winit::keyboard::NamedKey;

use super::*;
use crate::bots::onboard::{Mark, News, RoleSave, SetupBook, Stage};
use crate::bots::view::onboard as ui;

/// A Bot tab in front, its Bot read, its main conversation read and empty,
/// told that its setup is not over on this install, and laid out.
fn shown(bot: Bot) -> Fixture {
    let mut f = offered(bot);
    if let Some(read) = f.view.take_role_read() {
        assert_eq!(read.page, f.bot.config.role, "the Bot's own Role page");
        assert!(f.view.role_read(read.serial, Some(true)));
    }
    f.view.settle_setup();
    f
}

/// The same tab told that its setup is on offer, its Role page not read
/// yet: the read it asks for is still with the window.
fn offered(bot: Bot) -> Fixture {
    let mut f = opened_on_main(bot);
    f.view.tell_setup(false);
    f.view.settle_setup();
    f
}

/// The same tab before the window has said anything about its setup.
fn opened_on_main(bot: Bot) -> Fixture {
    let mut f = fixture();
    f.bot = bot;
    f.view.bot = Some(f.bot.id);
    f.view.chat = f.bot.main_chat;
    f.view.shown_pass(None);
    let seq = f.asked();
    f.answer(seq, f.bot.id, Ok(f.bot.clone()));
    f.view.set_home_name(Some("Research".into()));
    // Tall enough that the whole card shows without scrolling.
    f.view.set_viewport(1000.0, 1400.0);
    f.view
        .conversation_mut()
        .expect("the conversation is bound")
        .pretend_read(f.bot.main_chat);
    f.view.settle_setup();
    f
}

fn rect_of(f: &Fixture, key: Key) -> Option<Rect> {
    f.view
        .setup_frame(|frame| frame.node(key).map(|node| node.rect))
        .flatten()
}

fn drawn(f: &Fixture, key: Key) -> bool {
    rect_of(f, key).is_some()
}

/// A press where the control is, entered as the window enters it: through
/// the tab, to the chat, and back as the press the chat hands over.
fn press(f: &mut Fixture, key: Key) -> bool {
    f.view.settle_setup();
    let rect = rect_of(f, key).expect("the control is shown");
    let mut ws = koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t")
        .expect("a document");
    let pressed = View::mouse_down(
        &mut f.view,
        &mut ws,
        rect.x + rect.w * 0.5,
        rect.y + rect.h * 0.5,
        false,
    );
    View::mouse_up(&mut f.view, &mut ws);
    f.view.settle_setup();
    f.publishable("after a press on the setup");
    pressed
}

fn words(f: &Fixture, key: Words) -> String {
    koru_i18n::fill(t(f.view.lang, key), &["Scout"])
}

fn setup_controls(f: &Fixture) -> Vec<Control> {
    every_control(f.view.controls())
        .into_iter()
        .filter(|control| ui::is_setup_key(control.key))
        .collect()
}

fn control(f: &Fixture, key: Key) -> Control {
    setup_controls(f)
        .into_iter()
        .find(|control| control.key == key)
        .unwrap_or_else(|| panic!("{key:?} is not published"))
}

/// The published control `key` with what nests under it.
fn nested(controls: &[Control], key: Key) -> Option<Control> {
    controls.iter().find_map(|control| {
        if control.key == key {
            Some(control.clone())
        } else {
            nested(&control.children, key)
        }
    })
}

/// Answer the Library read step 2 sent, with `pages` as the Bot's Knowledge.
fn answer_library(f: &mut Fixture, pages: Vec<BotPage>) {
    assert!(f.view.shown_pass(None), "step 2 reads the Library");
    let seq = loop {
        if let Command::ReadLibrary { seq, .. } = f.commands.try_recv().expect("a Library read") {
            break seq;
        }
    };
    f.answers
        .send(Answer::Library {
            seq,
            bot: f.bot.id,
            result: Ok(BotLibraryView {
                bot_id: f.bot.id,
                revision: f.bot.revision,
                role: f.bot.config.role.clone(),
                knowledge: pages,
                memory: None,
            }),
        })
        .expect("the tab's end is open");
    assert!(f.view.poll());
    f.view.settle_setup();
}

/// **The greeting and the card show over a Bot's empty main conversation**,
/// and nowhere else: not before the window has said, not once the setup is
/// over on this install, not on a conversation that holds something, not on
/// another conversation of the Bot, not on an archived Bot, not under the
/// settings, and never in an ordinary chat.
#[test]
fn the_greeting_and_the_card_show_over_an_empty_main_conversation_only() {
    let f = shown(unpaused());
    assert!(f.view.setup_shown());
    for key in [
        ui::SETUP,
        ui::GREETING,
        ui::CARD,
        ui::SKIP_ALL,
        ui::STEP_ROLE,
        ui::STEP_CONTEXT,
        ui::ROLE_FIELD,
        ui::SAVE_ROLE,
        ui::SKIP_ROLE,
        ui::HINT,
    ] {
        assert!(drawn(&f, key), "{key:?} is drawn");
    }
    assert_eq!(
        published(&f.view, ui::GREETING),
        Some(words(&f, Words::BotsSetupGreeting))
    );
    assert!(words(&f, Words::BotsSetupGreeting).contains("Scout"));
    f.publishable("the setup shown");

    // Not before the window has said whether it was ended here.
    let untold = opened_on_main(unpaused());
    assert!(!untold.view.setup_shown());
    assert!(setup_controls(&untold).is_empty());

    // Not once it is over on this install.
    let mut over = opened_on_main(unpaused());
    over.view.tell_setup(true);
    over.view.settle_setup();
    assert!(!over.view.setup_shown());

    // Not on a conversation that already holds something.
    let mut busy = opened_on_main(unpaused());
    busy.view
        .conversation_mut()
        .expect("bound")
        .pretend_reply_running();
    busy.view.tell_setup(false);
    busy.view.settle_setup();
    assert!(!busy.view.setup_shown());
    assert_eq!(busy.view.take_setup_news(), None, "it never showed");

    // Not on another of the Bot's conversations.
    let mut other = shown(unpaused());
    other.view.chat = ChatId::new();
    other.view.settle_setup();
    assert!(!other.view.setup_shown());

    // Not on an archived Bot, whose history reads and nothing changes.
    let mut archived = unpaused();
    archived.lifecycle = BotLifecycle::Archived;
    let archived = shown(archived);
    assert!(!archived.view.setup_shown());

    // Not under the settings.
    let mut settings = shown(unpaused());
    settings.view.set_settings_open(true);
    settings.view.settle_setup();
    assert!(!settings.view.setup_shown());
    assert!(setup_controls(&settings).is_empty());
    settings.view.set_settings_open(false);
    settings.view.settle_setup();
    assert!(settings.view.setup_shown(), "and back when they close");

    // An ordinary chat asks for no room and gets none.
    let chat = ChatId::new();
    let mut ordinary = AiChatView::new();
    ordinary.pretend_read(chat);
    ordinary.set_area(Rect::new(0.0, 0.0, 800.0, 600.0));
    assert_eq!(ordinary.transcript_is_empty(), Some(true));
    assert_eq!(ordinary.hosted_empty_area(), None);
}

/// **A Role a person already wrote is never replaced, and a Role not yet
/// read is never taken for empty.** The card is offered only once the Bot's
/// Role page has read as empty: while the read is out, after it failed, and
/// when the page holds something, there is no card, no field and no Save a
/// press or a client could reach, and nothing is left for the window to
/// write.
#[test]
fn the_card_is_offered_only_for_an_empty_role_page() {
    use crate::bots::onboard::RoleKnown;
    for answer in [None, Some(None), Some(Some(false))] {
        let mut f = offered(unpaused());
        let read = f
            .view
            .take_role_read()
            .expect("the tab asks about its Role");
        assert!(f.view.take_role_read().is_none(), "once");
        assert_eq!(f.view.setup().role_known(), RoleKnown::Reading);
        if let Some(answer) = answer {
            assert!(f.view.role_read(read.serial, answer));
        }
        f.view.settle_setup();
        assert!(!f.view.setup_shown(), "{answer:?}: no card");
        assert!(
            setup_controls(&f).is_empty(),
            "{answer:?}: nothing published"
        );
        assert!(
            !f.view.accessibility_set_value(ui::ROLE_FIELD, "A new role"),
            "{answer:?}: no field"
        );
        assert!(
            !f.view.accessibility_action(ui::SAVE_ROLE, true),
            "{answer:?}: no Save"
        );
        assert!(f.view.take_role_write().is_none(), "{answer:?}: no write");
        assert!(
            f.view.take_role_read().is_none(),
            "{answer:?}: not asked twice"
        );
        f.publishable("no setup");
    }

    // A failed read is asked again when the tab is shown again.
    let mut f = offered(unpaused());
    let read = f.view.take_role_read().expect("a read");
    assert!(f.view.role_read(read.serial, None));
    f.view.hidden();
    f.view.settle_setup();
    let again = f.view.take_role_read().expect("asked again");
    assert_ne!(again.serial, read.serial);
    assert!(f.view.role_read(again.serial, Some(true)));
    f.view.settle_setup();
    assert!(f.view.setup_shown(), "an empty Role: the card, as before");

    // An archived Bot's Role is not even read.
    let mut archived = unpaused();
    archived.lifecycle = BotLifecycle::Archived;
    let mut archived = offered(archived);
    assert!(archived.view.take_role_read().is_none());
}

/// **The setup is no part of the conversation**: the conversation is still
/// read and empty under it, its composer is the ordinary composer and takes
/// typing while the card shows, and what is typed there is not the card's.
#[test]
fn the_setup_is_not_in_the_conversation_and_the_composer_still_types() {
    let mut f = shown(unpaused());
    let conversation = f.view.conversation().expect("bound");
    assert_eq!(
        conversation.transcript_is_empty(),
        Some(true),
        "no message was made of the greeting"
    );
    for typed in ["h", "i"] {
        assert!(
            key(&mut f.view, WinitKey::Character(typed.into()), Some(typed)),
            "{typed}: the composer's"
        );
    }
    assert_eq!(
        f.view.conversation_mut().expect("bound").composer_text(),
        "hi"
    );
    assert_eq!(f.view.setup().text(), "", "typed in the composer, not here");
    assert!(f.view.setup_shown(), "typing alone ends nothing");
    assert_eq!(f.view.take_setup_news(), None);
    while let Ok(command) = f.commands.try_recv() {
        assert!(
            !matches!(command, Command::Write { .. }),
            "showing the setup wrote something to the Bot service"
        );
    }
}

/// **An example fills step 1's text**, in the field and in what Save would
/// write, and leaves the keyboard in the field.
#[test]
fn an_example_fills_the_text() {
    let mut f = shown(unpaused());
    for (index, example) in ui::EXAMPLES.into_iter().enumerate() {
        assert!(press(&mut f, example));
        let filled = t(
            f.view.lang,
            crate::bots::onboard::Example::ALL[index].text(),
        );
        assert_eq!(f.view.setup().text(), filled);
        assert_eq!(control(&f, ui::ROLE_FIELD).value.as_deref(), Some(filled));
        assert_eq!(f.view.focus(), Some(ui::ROLE_FIELD));
    }
    assert!(
        f.view.take_role_write().is_none(),
        "an example saves nothing"
    );
    // The keyboard is the field's now: typing goes on from the example.
    assert!(key(&mut f.view, WinitKey::Character("!".into()), Some("!")));
    assert!(f.view.setup().text().ends_with('!'));
    assert_eq!(
        f.view.conversation_mut().expect("bound").composer_text(),
        "",
        "and not into the composer"
    );
}

/// **Save writes the Role once, shows pending, then saved; a failure is
/// said and can be retried.** The write is the Bot's own Role page and the
/// typed words; nothing goes to the Bot service.
#[test]
fn save_writes_the_role_once_and_a_failure_can_be_retried() {
    let mut f = shown(unpaused());
    assert!(control(&f, ui::SAVE_ROLE).disabled, "nothing typed yet");
    assert!(
        f.view
            .accessibility_set_value(ui::ROLE_FIELD, "Review my specs.")
    );
    f.view.settle_setup();
    assert!(!control(&f, ui::SAVE_ROLE).disabled);

    assert!(press(&mut f, ui::SAVE_ROLE));
    let write = f.view.take_role_write().expect("one write for the window");
    assert_eq!(write.bot, f.bot.id);
    assert_eq!(write.page, f.bot.config.role, "the Bot's own Role page");
    assert_eq!(write.markdown, "Review my specs.");
    assert!(!write.before.is_empty() && !write.after.is_empty());
    assert_eq!(f.view.setup().role_save(), RoleSave::Saving);
    assert_eq!(
        published(&f.view, ui::ROLE_STATUS),
        Some(t(f.view.lang, Words::BotsSaving).to_owned())
    );
    assert!(
        control(&f, ui::SAVE_ROLE).disabled,
        "pending: no second write"
    );
    press(&mut f, ui::SAVE_ROLE);
    assert!(f.view.take_role_write().is_none(), "written once");
    assert!(
        f.view.maintenance_deadline().is_some(),
        "the tab asks to be woken for the answer"
    );

    // The Store did not take it: said in words, and Save reads Retry.
    assert!(f.view.role_written(write.serial, false));
    f.view.settle_setup();
    assert_eq!(f.view.setup().stage(), Stage::Role);
    assert_eq!(
        published(&f.view, ui::ROLE_STATUS),
        Some(t(f.view.lang, Words::BotsSetupRoleFailed).to_owned())
    );
    let retry = control(&f, ui::SAVE_ROLE);
    assert_eq!(retry.label, t(f.view.lang, Words::BotsRetry));
    assert!(!retry.disabled);

    assert!(press(&mut f, ui::SAVE_ROLE));
    let again = f.view.take_role_write().expect("the retry");
    assert_ne!(again.serial, write.serial);
    assert_eq!(again.markdown, write.markdown);
    assert!(f.view.role_written(again.serial, true));
    f.view.settle_setup();
    assert_eq!(
        f.view.setup().stage(),
        Stage::Context,
        "saved: on to step 2"
    );
    assert_eq!(
        control(&f, ui::STEP_ROLE).label,
        koru_i18n::fill(
            t(f.view.lang, Words::BotsSetupStepIsDone),
            &["1", "2", t(f.view.lang, Words::BotsSetupStepRole)],
        ),
        "a tick, and the word for it"
    );
    // Never a Bot command: the Role is a page, written over the Store.
    while let Ok(command) = f.commands.try_recv() {
        assert!(
            !matches!(command, Command::Write { .. }),
            "saving the Role sent the Bot service a write"
        );
    }
}

/// **Step 2 adds a page through the Library's own mechanism**: the tab's
/// page picker, a Knowledge draft, and `SetKnowledge` sent at once. The
/// selected pages the Bot may edit are untouched, and the window's context
/// column lists the page once the owner has it.
#[test]
fn step_two_adds_a_knowledge_page_through_the_library() {
    use koru_editor::components::entity_picker::{DocumentChoice, WorkspaceChoice};
    let mut f = shown(unpaused());
    let access = f.view.settings().pages_shown().to_vec();
    assert!(press(&mut f, ui::SKIP_ROLE));
    assert_eq!(f.view.setup().stage(), Stage::Context);
    assert!(
        control(&f, ui::BROWSE).disabled,
        "the Library is not read yet"
    );
    answer_library(&mut f, Vec::new());
    assert_eq!(
        published(&f.view, ui::CONTEXT_HINT),
        Some(words(&f, Words::BotsSetupContextHint))
    );
    assert!(!control(&f, ui::BROWSE).disabled);

    assert!(press(&mut f, ui::BROWSE));
    assert!(f.view.page_picker_open(), "the tab's existing picker");
    let home = koru_core::WorkspaceId::from_uuid(f.bot.home_workspace.as_uuid());
    f.view.set_picker_context(vec![WorkspaceChoice {
        profile: koru_core::ProfileId::new(),
        profile_name: "Me".into(),
        id: home,
        name: "Research".into(),
    }]);
    let document = koru_core::DocumentId::new();
    f.view.set_picker_results(
        vec![DocumentChoice {
            target: koru_editor::trigger::DocumentTarget {
                document,
                node: None,
            },
            title: "Research notes".into(),
            breadcrumb: String::new(),
            is_file: false,
        }],
        false,
    );
    assert!(key(&mut f.view, named(NamedKey::Enter), None));
    assert!(!f.view.page_picker_open());
    assert_eq!(f.view.focus(), Some(ui::BROWSE), "the keyboard comes back");

    let page = BotPage {
        workspace: f.bot.home_workspace,
        document: koru_ai_core::DocumentId::from_uuid(document.as_uuid()),
    };
    let (request, command) = f.wrote();
    assert_eq!(
        command,
        BotCommand::SetKnowledge {
            request_id: request,
            bot_id: f.bot.id,
            expected_revision: f.bot.revision,
            pages: vec![page.clone()],
        },
        "the Library's own save, sent at once"
    );
    assert_eq!(
        f.view.settings().pages_shown(),
        access,
        "no grant was added"
    );
    f.view.settle_setup();
    assert_eq!(
        published(&f.view, ui::CONTEXT_STATUS),
        Some(t(f.view.lang, Words::BotsSaving).to_owned())
    );
    assert!(control(&f, ui::BROWSE).disabled, "one save at a time");

    f.answers
        .send(Answer::Wrote {
            bot: f.bot.id,
            request,
            result: Ok(BotResponse::KnowledgeSaved {
                receipt: BotKnowledgeReceipt {
                    bot_id: f.bot.id,
                    request_id: request,
                    revision: f.bot.revision + 1,
                    pages: vec![page.clone()],
                },
            }),
        })
        .expect("the tab's end is open");
    assert!(f.view.poll());
    f.view.settle_setup();
    assert_eq!(f.view.library.pages(), [page.clone()]);
    assert!(
        setup_controls(&f)
            .iter()
            .any(|control| control.key.part == ui::PAGE_PART),
        "the page is listed in the step"
    );
    // The window's context column shows it as Knowledge.
    let column = f.view.context_library_data(1).expect("a Bot's context");
    assert!(column.pages.iter().any(|listed| listed.page == page));
    assert_eq!(f.view.setup().mark(Stage::Context, true), Mark::Current);

    assert!(press(&mut f, ui::DONE));
    assert_eq!(f.view.setup().stage(), Stage::Done);
    assert_eq!(f.view.take_setup_news(), Some(News::Dismissed));
}

/// **A failed Knowledge save is said and can be retried** from the step.
#[test]
fn a_failed_context_save_is_said_and_retried() {
    let mut f = shown(unpaused());
    press(&mut f, ui::SKIP_ROLE);
    answer_library(&mut f, Vec::new());
    let page = BotPage {
        workspace: f.bot.home_workspace,
        document: koru_ai_core::DocumentId::new(),
    };
    assert!(f.view.setup_add_page(page.clone()));
    let (request, command) = f.wrote();
    f.view.library.unsent(request);
    f.view.settle_setup();
    assert_eq!(
        published(&f.view, ui::CONTEXT_STATUS),
        Some(t(f.view.lang, Words::BotsErrUnreachable).to_owned())
    );
    assert!(press(&mut f, ui::RETRY_CONTEXT));
    assert_eq!(f.wrote(), (request, command), "the same save again");
}

/// **Skip, Finish and a first message each end the setup**, and what the
/// window's book holds of it survives the session file: the same Bot's tab,
/// made again and told from the book as read back, shows no card.
#[test]
fn skip_finish_and_a_first_message_end_the_setup_and_it_stays_ended() {
    // *Skip setup*: the card says the Bot is ready and offers the way back.
    let mut f = shown(unpaused());
    assert!(press(&mut f, ui::SKIP_ALL));
    assert_eq!(f.view.take_setup_news(), Some(News::Dismissed));
    assert!(f.view.setup_shown(), "the finished card, for this showing");
    assert!(drawn(&f, ui::READY) && drawn(&f, ui::AGAIN));
    assert!(!drawn(&f, ui::SKIP_ALL) && !drawn(&f, ui::ROLE_FIELD));
    assert!(!drawn(&f, ui::HINT));
    assert_eq!(
        published(&f.view, ui::READY),
        Some(words(&f, Words::BotsSetupReady))
    );

    // The book, through its line and back: a restart of the view state.
    let mut book = SetupBook::default();
    assert!(book.note(f.bot.id, true));
    let read_back = SetupBook::parse(&book.to_storage());
    let mut again = opened_on_main(f.bot.clone());
    again.view.tell_setup(read_back.contains(again.bot.id));
    again.view.settle_setup();
    assert!(!again.view.setup_shown(), "ended stays ended");
    assert!(setup_controls(&again).is_empty());

    // The way back in opens it again, here and in the book.
    assert!(press(&mut f, ui::AGAIN));
    assert_eq!(f.view.take_setup_news(), Some(News::Reopened));
    assert_eq!(f.view.setup().stage(), Stage::Role);
    assert!(drawn(&f, ui::ROLE_FIELD));

    // Finish: step 2's Done, and its Skip, are the same end.
    for end in [ui::DONE, ui::SKIP_CONTEXT] {
        let mut f = shown(unpaused());
        assert!(press(&mut f, ui::STEP_CONTEXT));
        assert!(press(&mut f, end));
        assert_eq!(f.view.take_setup_news(), Some(News::Dismissed));
        assert_eq!(f.view.setup().stage(), Stage::Done);
    }

    // A first message: the card gives way at once, for good.
    let mut f = shown(unpaused());
    f.view
        .conversation_mut()
        .expect("bound")
        .pretend_reply_running();
    assert!(
        f.view.shown_pass(None),
        "the pass sees the conversation move"
    );
    f.view.settle_setup();
    assert!(!f.view.setup_shown());
    assert_eq!(f.view.take_setup_news(), Some(News::Dismissed));
    assert!(setup_controls(&f).is_empty());
}

/// **The steps are a list a reader can follow**: two items, each named with
/// its number, how many there are, and whether it is done, current or not
/// done; the current one selected; each reachable by a press.
#[test]
fn the_stepper_says_its_states_and_every_step_is_reachable() {
    let mut f = shown(unpaused());
    let list = nested(&f.view.controls(), ui::STEPS_LIST).expect("the steps are published");
    assert_eq!(list.role, ControlRole::List);
    assert_eq!(list.label, t(f.view.lang, Words::BotsSetupSteps));
    assert_eq!(
        list.children
            .iter()
            .map(|step| step.key)
            .collect::<Vec<_>>(),
        [ui::STEP_ROLE, ui::STEP_CONTEXT],
        "two items, in order, under the list"
    );
    // The list is inside the group that names the whole setup.
    let group = nested(&f.view.controls(), ui::SETUP).expect("the setup is a group");
    assert_eq!(group.role, ControlRole::Group);
    assert_eq!(group.label, words(&f, Words::BotsSetupTitle));
    assert!(nested(&group.children, ui::STEPS_LIST).is_some());
    let said = |f: &Fixture, key: Key| control(f, key).label;
    let named_as = |f: &Fixture, state: Words, number: &str, step: Words| {
        koru_i18n::fill(t(f.view.lang, state), &[number, "2", t(f.view.lang, step)])
    };
    assert_eq!(
        said(&f, ui::STEP_ROLE),
        named_as(
            &f,
            Words::BotsSetupStepIsCurrent,
            "1",
            Words::BotsSetupStepRole
        )
    );
    assert_eq!(
        said(&f, ui::STEP_CONTEXT),
        named_as(
            &f,
            Words::BotsSetupStepIsWaiting,
            "2",
            Words::BotsSetupStepContext
        )
    );
    assert_eq!(control(&f, ui::STEP_ROLE).role, ControlRole::ListItem);
    assert_eq!(control(&f, ui::STEP_ROLE).selected, Some(true));
    assert_eq!(control(&f, ui::STEP_CONTEXT).selected, Some(false));

    assert!(press(&mut f, ui::STEP_CONTEXT));
    assert_eq!(f.view.setup().stage(), Stage::Context);
    assert!(drawn(&f, ui::BROWSE) && !drawn(&f, ui::ROLE_FIELD));
    assert_eq!(control(&f, ui::STEP_CONTEXT).selected, Some(true));
    // Skipped, not saved: step 1 keeps its number and says not done.
    assert_eq!(
        said(&f, ui::STEP_ROLE),
        named_as(
            &f,
            Words::BotsSetupStepIsWaiting,
            "1",
            Words::BotsSetupStepRole
        )
    );
    assert!(press(&mut f, ui::STEP_ROLE));
    assert_eq!(f.view.setup().stage(), Stage::Role);
    assert!(drawn(&f, ui::ROLE_FIELD));
    assert_eq!(
        f.view.take_setup_news(),
        None,
        "moving between steps ends nothing"
    );
}

/// **Every control of the setup is named and reachable from the keyboard**:
/// `F6` lands on the header, Tab walks on into the card in the order it is
/// drawn, Enter presses, and Escape gives the keyboard back to the
/// conversation.
#[test]
fn the_keyboard_reaches_every_control_and_escape_gives_it_back() {
    let mut f = shown(unpaused());
    let order = f.view.setup_focus_order();
    assert_eq!(
        order,
        [
            ui::SKIP_ALL,
            ui::STEP_ROLE,
            ui::STEP_CONTEXT,
            ui::ROLE_FIELD,
            ui::EXAMPLES[0],
            ui::EXAMPLES[1],
            ui::EXAMPLES[2],
            ui::SKIP_ROLE,
        ],
        "the order it is drawn in; Save waits for words"
    );
    for control in setup_controls(&f) {
        assert!(
            !control.label.trim().is_empty(),
            "{:?} has no name",
            control.key
        );
    }
    assert_eq!(control(&f, ui::ROLE_FIELD).role, ControlRole::TextInput);
    assert_eq!(
        control(&f, ui::ROLE_FIELD).label,
        words(&f, Words::BotsSetupRoleLabel)
    );

    assert!(f.view.focus_header(), "F6 enters the first drawn control");
    assert_eq!(f.view.focus(), Some(ui::SKIP_ALL));
    let mut walked = vec![ui::SKIP_ALL];
    for _ in 0..40 {
        assert!(key(&mut f.view, named(NamedKey::Tab), None));
        let focus = f.view.focus().expect("the walk keeps the keyboard");
        if ui::is_setup_key(focus) {
            walked.push(focus);
        }
        if focus == ui::STEP_CONTEXT {
            break;
        }
    }
    assert_eq!(walked, [ui::SKIP_ALL, ui::STEP_ROLE, ui::STEP_CONTEXT]);
    f.view.settle_setup();
    assert!(control(&f, ui::STEP_CONTEXT).focused, "the published focus");

    assert!(key(&mut f.view, named(NamedKey::Enter), None));
    f.view.settle_setup();
    assert_eq!(f.view.setup().stage(), Stage::Context);
    assert_eq!(
        f.view.focus(),
        Some(ui::STEP_CONTEXT),
        "it stays where it was"
    );

    // Back to step 1, and into its field: typing there is the field's.
    assert!(f.view.accessibility_action(ui::STEP_ROLE, true));
    f.view.settle_setup();
    assert!(f.view.accessibility_action(ui::ROLE_FIELD, false));
    assert!(key(&mut f.view, WinitKey::Character("a".into()), Some("a")));
    assert_eq!(f.view.setup().text(), "a");
    assert_eq!(
        f.view.conversation_mut().expect("bound").composer_text(),
        ""
    );

    assert!(key(&mut f.view, named(NamedKey::Escape), None));
    assert_eq!(f.view.focus(), None, "Escape gives the keyboard back");
    assert!(key(&mut f.view, WinitKey::Character("b".into()), Some("b")));
    assert_eq!(
        f.view.conversation_mut().expect("bound").composer_text(),
        "b",
        "and typing is the composer's again"
    );
    assert_eq!(f.view.setup().text(), "a");
}

/// **A client's click and its text are a person's**: a click on a control
/// does what a press does, an inert one does nothing, and one that is not
/// drawn is refused.
#[test]
fn an_assistive_technology_drives_the_setup() {
    let mut f = shown(unpaused());
    assert!(
        !f.view.accessibility_action(ui::SAVE_ROLE, true),
        "inert while there are no words"
    );
    assert!(f.view.take_role_write().is_none());
    assert!(!f.view.accessibility_action(ui::BROWSE, true), "not drawn");
    assert!(
        !f.view.accessibility_set_value(ui::BROWSE, "x"),
        "not a field"
    );
    assert!(
        f.view
            .accessibility_set_value(ui::ROLE_FIELD, "Keep a digest.")
    );
    f.view.settle_setup();
    assert!(f.view.accessibility_action(ui::SAVE_ROLE, true));
    assert_eq!(
        f.view.take_role_write().map(|write| write.markdown),
        Some("Keep a digest.".to_owned())
    );
    assert!(f.view.accessibility_action(ui::SKIP_ALL, true));
    f.view.settle_setup();
    assert_eq!(f.view.setup().stage(), Stage::Done);
    assert!(
        !f.view.accessibility_set_value(ui::ROLE_FIELD, "late"),
        "the field left with its step"
    );
}

/// **Every language fits the room it is given**: nothing the setup draws
/// runs out of the conversation's column, every published control has a
/// name, and the frame reports nothing miscomposed. A short room scrolls
/// instead of cutting the card off.
#[test]
fn the_setup_fits_its_room_in_every_language_and_scrolls_when_short() {
    for lang in Lang::ALL {
        let mut f = shown(unpaused());
        f.view.lang = lang;
        // A narrow column: the longest words have the least room.
        f.view.set_viewport(560.0, 1400.0);
        f.view.settle_setup();
        for stage in [Stage::Role, Stage::Context, Stage::Done] {
            match stage {
                Stage::Role => {}
                Stage::Context => {
                    assert!(press(&mut f, ui::STEP_CONTEXT));
                }
                Stage::Done => {
                    assert!(press(&mut f, ui::SKIP_ALL));
                }
            }
            assert_eq!(f.view.setup().stage(), stage);
            let room = rect_of(&f, ui::SETUP).expect("the setup's room");
            f.view
                .setup_frame(|frame| {
                    assert!(
                        frame.diagnostics.is_empty(),
                        "{} {stage:?}: {:?}",
                        lang.tag(),
                        frame.diagnostics
                    );
                    for node in frame.nodes.iter().filter(|node| node.key.is_some()) {
                        assert!(
                            node.rect.x >= room.x - 0.5
                                && node.rect.x + node.rect.w <= room.x + room.w + 0.5,
                            "{} {stage:?}: {} runs out of the room: {:?} in {room:?}",
                            lang.tag(),
                            node.name,
                            node.rect
                        );
                    }
                })
                .expect("the setup shows");
            for control in setup_controls(&f) {
                assert!(!control.label.trim().is_empty(), "{}", lang.tag());
            }
            f.publishable("the setup in every language");
        }
    }

    // A short window: the card is taller than its room, and the wheel and
    // the keyboard's walk bring the rest of it into view.
    let mut f = shown(unpaused());
    f.view.set_viewport(700.0, 460.0);
    f.view.settle_setup();
    assert!(f.view.setup_shown());
    let (extent, room) = f
        .view
        .setup_frame(|frame| {
            let node = frame.node(ui::SETUP).expect("the viewport");
            (
                node.scroll.map_or(0.0, |(extent, _)| extent),
                node.viewport.unwrap_or(node.rect),
            )
        })
        .expect("the setup shows");
    assert!(extent > room.h, "the card overflows a short room");
    let middle = (room.x + room.w * 0.5, room.y + room.h * 0.5);
    View::mouse_move(&mut f.view, middle.0, middle.1);
    View::scroll_by(&mut f.view, MouseScrollDelta::LineDelta(0.0, -3.0));
    let scrolled = f
        .view
        .setup_frame(|frame| frame.node(ui::SETUP).and_then(|node| node.scroll))
        .flatten()
        .map_or(0.0, |(_, offset)| offset);
    assert!(scrolled > 0.0, "the wheel scrolls the setup");
    assert!(
        scrolled <= extent - room.h + 0.5,
        "and no further than it goes"
    );
}
