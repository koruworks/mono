// SPDX-License-Identifier: MPL-2.0

//! **Creating a Bot in its tab** (Bots P1.8 S8), driven through the tab
//! with a link and a catalog link the test answers.

use koru_ai_core::bots::{
    BotCreation, BotCreationLeftovers, BotCreationPhase, BotCreationReason, BotCreationStage,
};
use koru_ai_ui::agent_catalog::config::picker::{RailEntry, Readiness};
use koru_ai_ui::agent_catalog::{CatalogAgent, CatalogListing, CatalogMode, CatalogModel};

use super::*;
use crate::bots::catalog::{self, Catalog, CatalogLink};
use crate::bots::view::create::*;

struct Create {
    view: BotView,
    commands: Receiver<Command>,
    answers: SyncSender<Answer>,
    home: WorkspaceId,
    catalog_commands: Receiver<catalog::Command>,
    catalog_answers: SyncSender<catalog::Answer>,
    /// What every agent's listing answers.
    listing: CatalogListing,
}

fn agent(id: &str, label: &str) -> CatalogAgent {
    CatalogAgent {
        id: AiProviderId::parse(id).expect("a provider id"),
        key: Some(id.trim_start_matches("agent-").to_owned()),
        label: label.into(),
        mark: koru_draw::brand::ProfileMark::Glyph(koru_draw::icons::ROBOT),
    }
}

fn listing() -> CatalogListing {
    CatalogListing {
        models: vec![CatalogModel {
            id: "gpt-5".into(),
            group: "openai".into(),
            name: Some("GPT-5".into()),
            description: None,
            group_name: None,
            legacy: false,
            levels: None,
            context: None,
            fast: Some(false),
        }],
        modes: vec![
            CatalogMode {
                id: "auto-review".into(),
                name: None,
                description: None,
            },
            CatalogMode {
                id: "ask".into(),
                name: None,
                description: None,
            },
        ],
        default_mode: Some("auto-review".into()),
    }
}

/// A shown create tab whose catalog answered with Codex (ready) and Claude
/// (signed out), with Codex the person's default agent.
fn shown() -> Create {
    shown_with_default("codex")
}

fn shown_with_default(default_agent: &str) -> Create {
    shown_with(default_agent, listing())
}

/// A shown create tab whose agents list `listing`.
fn shown_with(default_agent: &str, listing: CatalogListing) -> Create {
    let (link, commands, answers) = BotsLink::test_pair();
    let home = WorkspaceId::new();
    let mut view = BotView::creating_with_link(home, link);
    view.set_viewport(800.0, 2400.0);
    let (catalog_link, catalog_commands, catalog_answers) = CatalogLink::test_pair();
    view.catalog = Box::new(Catalog::with_link(catalog_link));
    view.catalog.shown();
    view.set_agent_settings(&catalog::AgentSettings {
        default_agent: Some(default_agent.into()),
        ..Default::default()
    });
    let mut c = Create {
        view,
        commands,
        answers,
        home,
        catalog_commands,
        catalog_answers,
        listing,
    };
    c.view.shown_pass(None);
    c.serve_catalog();
    c
}

impl Create {
    /// Answer every catalog read the tab asked for, until it asks no more.
    fn serve_catalog(&mut self) {
        for _ in 0..4 {
            self.serve_catalog_once();
            self.view.shown_pass(None);
        }
    }

    fn serve_catalog_once(&mut self) {
        let (catalog_commands, catalog_answers) = (&self.catalog_commands, &self.catalog_answers);
        let served = &self.listing;
        while let Ok(command) = catalog_commands.try_recv() {
            match command {
                catalog::Command::Agents { seq } => catalog_answers
                    .send(catalog::Answer::Agents {
                        seq,
                        result: Ok(vec![
                            agent("agent-codex", "Codex"),
                            agent("agent-claude", "Claude"),
                        ]),
                    })
                    .unwrap(),
                catalog::Command::Readiness { seq, agent } => catalog_answers
                    .send(catalog::Answer::Readiness {
                        seq,
                        result: Ok(if agent.as_str() == "agent-codex" {
                            Readiness::Ready
                        } else {
                            Readiness::SignedOut
                        }),
                        agent,
                    })
                    .unwrap(),
                catalog::Command::Listing { seq, agent, .. } => catalog_answers
                    .send(catalog::Answer::Listing {
                        seq,
                        agent,
                        result: Ok(served.clone()),
                    })
                    .unwrap(),
            }
        }
    }
}

impl Create {
    fn form(&self) -> &crate::bots::creation::CreateForm {
        self.view.create_form().expect("a create tab")
    }

    /// The one creation command the tab just sent, by its draft.
    fn sent(&self) -> BotCommand {
        match self.commands.try_recv().expect("a command was sent") {
            Command::Write {
                bot,
                request,
                command,
            } => {
                assert_eq!(bot, self.form().bot(), "by the BotId the draft reserves");
                assert_eq!(request, self.form().request(), "by the draft's request");
                *command
            }
            _ => panic!("a creation command"),
        }
    }

    fn nothing_sent(&self) {
        assert!(self.commands.try_recv().is_err(), "nothing was sent");
    }

    fn reply(&mut self, creation: BotCreation) {
        self.answers
            .send(Answer::Wrote {
                bot: creation.bot_id,
                request: creation.request_id,
                result: Ok(BotResponse::Creation { creation }),
            })
            .expect("the tab's end is open");
        self.view.poll();
    }

    fn creation(&self, stage: BotCreationStage) -> BotCreation {
        BotCreation {
            request_id: self.form().request(),
            bot_id: self.form().bot(),
            home_workspace: self.home,
            name: "Scout".into(),
            stage,
            phase: BotCreationPhase::RolePending,
            reason: None,
            retryable: false,
            role_page: None,
            role_page_unconfirmed: false,
            folder_binding: None,
            left_behind: None,
            bot: None,
        }
    }

    /// Type `text` into the field the keyboard is on.
    fn type_text(&mut self, text: &str) {
        for ch in text.chars() {
            let typed = ch.to_string();
            key(
                &mut self.view,
                WinitKey::Character(typed.as_str().into()),
                Some(&typed),
            );
        }
    }

    /// Tab until the keyboard is on `target`.
    fn tab_to(&mut self, target: Key) {
        for _ in 0..80 {
            if self.view.focus() == Some(target) {
                return;
            }
            key(&mut self.view, named(winit::keyboard::NamedKey::Tab), None);
        }
        panic!("{target:?} is not in the keyboard's walk");
    }

    /// A ready draft: a name typed, a role chosen from an example.
    fn fill(&mut self) {
        assert!(self.view.focus_header(), "F6 lands on the form");
        assert_eq!(self.view.focus(), Some(NAME_FIELD), "on its name first");
        self.type_text("Scout");
        assert!(
            self.view
                .perform(Act::Example(crate::bots::creation::Example::Research))
        );
    }
}

fn all_controls(view: &BotView) -> Vec<Control> {
    fn walk(controls: &[Control], out: &mut Vec<Control>) {
        for control in controls {
            out.push(control.clone());
            walk(&control.children, out);
        }
    }
    let mut out = Vec::new();
    walk(&view.controls(), &mut out);
    out
}

/// **A create tab is a form, found by no conversation**, named *New bot*
/// until it is named; its controls are reachable by the keyboard through
/// the settings' own walk, in reading order, and every one has a name.
#[test]
fn the_create_form_is_reachable_by_keyboard_and_every_control_is_named() {
    let mut c = shown();
    let lang = c.view.lang;
    assert!(c.view.is_creating());
    assert_eq!(c.view.conversation_id(), None, "no conversation yet");
    assert_eq!(c.view.bot(), None);
    assert_eq!(c.view.opening(), None, "not a tab finding out whose it is");
    assert_eq!(c.view.label(), t(lang, Words::BotsNewBot));
    assert!(c.view.header_stop());
    c.fill();
    assert_eq!(c.view.label(), "Scout", "the tab is named as it is named");
    let order = c.view.focus_order();
    let at = |key: Key| {
        order
            .iter()
            .position(|k| *k == key)
            .unwrap_or_else(|| panic!("{key:?}"))
    };
    assert!(at(NAME_FIELD) < at(ROLE_CHOICE));
    assert!(at(ROLE_CHOICE) < at(TITLE_FIELD));
    assert!(at(TITLE_FIELD) < at(TEXT_FIELD));
    assert!(at(TEXT_FIELD) < at(EXAMPLE_OWN));
    assert!(at(EXAMPLE_ASSESSMENT) < at(region::MODEL_CHIP));
    assert!(at(region::MODEL_CHIP) < at(CHANGE_ACCESS));
    assert!(at(CHANGE_ACCESS) < at(CHOOSE_FOLDER));
    assert!(at(CHOOSE_FOLDER) < at(CREATE_BUTTON), "Create last");
    for control in all_controls(&c.view) {
        assert!(
            !control.label.trim().is_empty(),
            "{:?} has a name",
            control.key
        );
    }
    let controls = all_controls(&c.view);
    let field = |key: Key| {
        controls
            .iter()
            .find(|control| control.key == key)
            .unwrap_or_else(|| panic!("{key:?} is published"))
            .clone()
    };
    assert_eq!(field(NAME_FIELD).role, ControlRole::TextInput);
    assert_eq!(field(NAME_FIELD).value.as_deref(), Some("Scout"));
    assert_eq!(
        field(TITLE_FIELD).value.as_deref(),
        Some("Scout"),
        "follows the name"
    );
    assert_eq!(
        field(TEXT_FIELD).value.as_deref(),
        Some(t(lang, Words::BotsExampleResearchText))
    );
    let choice = field(ROLE_CHOICE);
    assert_eq!(
        choice.children.len(),
        2,
        "write a role | use an existing page"
    );
    assert_eq!(choice.children[0].selected, Some(true));
    assert_eq!(field(EXAMPLES_NOTE).label, t(lang, Words::BotsExamplesNote));
    assert_eq!(field(ACCESS_SUMMARY).role, ControlRole::Label);
    assert_eq!(
        field(FOLDER_PATH).label,
        t(lang, Words::BotsFolderManaged),
        "managed by default, in words"
    );
    assert_eq!(
        field(AGENT_FIXED_NOTE).label,
        t(lang, Words::BotsAgentFixed)
    );
}

/// **Create sends one `CreateBotWithRole`** with the draft's request and
/// BotId, the agent the catalog preferred at its *Default* model, the
/// runtime's asking mode — never the person's default permission — the
/// approved access default and the managed folder; **a duplicate press
/// sends nothing**.
#[test]
fn create_sends_one_creation_and_a_duplicate_press_sends_nothing() {
    let mut c = shown();
    c.fill();
    c.tab_to(CREATE_BUTTON);
    key(&mut c.view, named(winit::keyboard::NamedKey::Enter), None);
    let BotCommand::CreateBotWithRole {
        home_workspace,
        role,
        folder,
        config,
        access,
        selected_pages,
        ..
    } = c.sent()
    else {
        panic!("a create");
    };
    assert_eq!(home_workspace, c.home);
    assert!(
        matches!(role, koru_ai_core::bots::BotRoleInput::Authored { ref title, .. } if title == "Scout")
    );
    assert_eq!(folder, koru_ai_core::bots::BotFolderChoice::Managed {});
    assert_eq!(config.name, "Scout");
    assert_eq!(
        config.model.provider.as_str(),
        "agent-codex",
        "the default agent"
    );
    assert_eq!(config.model.requested_model, "default");
    assert_eq!(
        config.mode.as_deref(),
        Some("ask"),
        "the asking mode, not the agent's own start (auto-review)"
    );
    assert_eq!(access, BotAccess::Default);
    assert!(selected_pages.is_empty());
    // *Creating…*, and nothing more for a second press.
    assert!(c.keys_include(CREATING));
    assert!(!c.view.perform(Act::Create));
    c.nothing_sent();
    c.reply(c.creation(BotCreationStage::Pending));
    assert!(!c.view.perform(Act::Create));
    c.nothing_sent();
}

impl Create {
    fn keys_include(&self, key: Key) -> bool {
        self.view.with_frame(|_, frame| frame.contains(key))
    }
}

/// **Any registered agent runtime may be chosen**: the rail shows them all;
/// the default agent is kept and named when it is not ready — not refused,
/// and never swapped for another behind the person's back; choosing another
/// starts on its own defaults. No API-access provider is among them: the
/// catalog lists agent runtimes only.
#[test]
fn any_runtime_is_offered_and_one_that_is_not_ready_is_named() {
    let mut c = shown_with_default("claude");
    c.fill();
    let claude = AiProviderId::parse("agent-claude").unwrap();
    let codex = AiProviderId::parse("agent-codex").unwrap();
    assert_eq!(
        c.form().model_shown().unwrap().model.provider,
        claude,
        "the default"
    );
    let words = c.view.agent_readiness_words().expect("Claude is not ready");
    assert!(words.contains("Claude"), "{words}");
    assert!(
        c.form().problem().is_none(),
        "not ready is said, not refused"
    );
    assert!(c.view.model_chip_words().starts_with("Claude"));

    let mut draft = c.form().model_shown().unwrap();
    draft.mode = Some("auto-review".into());
    c.view.create_form_mut().unwrap().edit_model(draft);
    assert!(c.view.perform(Act::OpenModelPicker));
    let (hidden, rail) = c
        .view
        .agent_picker_read(|picker| (picker.rail_hidden(), picker.rail()))
        .expect("the picker is up");
    assert!(!hidden, "the agent is chosen here");
    let agents: Vec<String> = rail
        .iter()
        .filter_map(|entry| match entry {
            RailEntry::Agent(id) => Some(id.as_str().to_owned()),
            _ => None,
        })
        .collect();
    assert_eq!(agents, ["agent-codex", "agent-claude"]);
    assert!(
        agents.iter().all(|id| id.starts_with("agent-")),
        "runtimes only"
    );
    c.view
        .agent_picker_run(|view| view.select_rail_entry(RailEntry::Agent(codex.clone())));
    c.serve_catalog();
    let rows = c.view.agent_picker_read(|picker| picker.rows()).unwrap();
    let gpt = rows
        .iter()
        .position(|row| {
            matches!(row, koru_ai_ui::agent_catalog::config::picker::Row::Model(id) if id == "gpt-5")
        })
        .expect("Codex's GPT-5");
    c.view.agent_picker_run(|view| view.choose_row(gpt));
    let draft = c.form().model_shown().unwrap();
    assert_eq!(draft.model.provider, codex);
    assert_eq!(draft.model.requested_model, "gpt-5");
    assert_eq!(draft.mode, None, "Claude's mode is not carried to Codex");
    assert!(c.view.agent_readiness_words().is_none(), "Codex is ready");
}

/// **A hidden create tab sends nothing**; shown again, a creation it sent
/// is read once.
#[test]
fn a_hidden_create_tab_sends_nothing_and_reads_once_when_shown() {
    let mut c = shown();
    c.fill();
    assert!(c.view.perform(Act::Create));
    let _ = c.sent();
    c.reply(c.creation(BotCreationStage::Pending));
    c.view.hidden();
    let mut ws = koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t")
        .expect("a document");
    let later = std::time::Instant::now() + std::time::Duration::from_secs(10);
    assert!(!View::maintain(&mut c.view, &mut ws, later));
    c.nothing_sent();
    c.view.shown_pass(None);
    assert!(
        matches!(c.sent(), BotCommand::ReadBotCreation { .. }),
        "read once"
    );
    c.nothing_sent();
}

/// **When it stops: the reason in words, Retry only where it can finish**;
/// and a role page the Store may have made is shown as unconfirmed.
#[test]
fn a_stopped_creation_says_why_offers_retry_and_shows_the_page_unconfirmed() {
    let mut c = shown();
    c.fill();
    assert!(c.view.perform(Act::Create));
    let _ = c.sent();
    let mut stopped = c.creation(BotCreationStage::Incomplete);
    stopped.reason = Some(BotCreationReason::StoreUnavailable);
    stopped.retryable = true;
    stopped.role_page = Some(BotPage {
        workspace: c.home,
        document: koru_ai_core::DocumentId::new(),
    });
    stopped.role_page_unconfirmed = true;
    c.reply(stopped.clone());
    let lang = c.view.lang;
    let labels: Vec<String> = all_controls(&c.view).into_iter().map(|c| c.label).collect();
    for words in [
        Words::BotsCreateStoreUnavailable,
        Words::BotsCreateIncomplete,
        Words::BotsRolePageUnconfirmed,
    ] {
        assert!(labels.iter().any(|l| l == t(lang, words)), "{words:?}");
    }
    assert!(c.keys_include(RETRY) && c.keys_include(ABANDON));
    assert!(
        !c.keys_include(CREATE_BUTTON),
        "Create is not offered while held"
    );
    assert!(c.view.perform(Act::Retry));
    assert_eq!(
        c.sent(),
        BotCommand::RetryBotCreation {
            request_id: c.form().request()
        }
    );
    // Terminal: Retry is not offered, Abandon is.
    stopped.reason = Some(BotCreationReason::RoleMissing);
    stopped.retryable = false;
    c.reply(stopped);
    assert!(!c.keys_include(RETRY));
    assert!(c.keys_include(ABANDON));
}

/// **Abandon is asked first**, deletes nothing, and its answer names what
/// it left — the page, unconfirmed, with *Open role page*, and the folder.
/// The keyboard comes back to *Abandon* when the question is cancelled.
#[test]
fn abandon_is_asked_first_and_names_what_it_left() {
    let mut c = shown();
    c.fill();
    assert!(c.view.perform(Act::Create));
    let _ = c.sent();
    let mut stopped = c.creation(BotCreationStage::Incomplete);
    stopped.reason = Some(BotCreationReason::RoleMissing);
    c.reply(stopped);
    c.tab_to(ABANDON);
    key(&mut c.view, named(winit::keyboard::NamedKey::Enter), None);
    c.nothing_sent();
    assert_eq!(
        c.view.take_abandon_ask().as_deref(),
        Some("Scout"),
        "asked about"
    );
    assert_eq!(c.view.take_abandon_ask(), None, "once");
    // Cancelled: nothing sent, the keyboard back on *Abandon*.
    c.view.focus = None;
    c.view.dialog_closed();
    assert_eq!(c.view.focus(), Some(ABANDON));
    // Asked again and confirmed.
    assert!(c.view.perform(Act::Abandon));
    assert!(c.view.abandon_confirmed());
    assert_eq!(
        c.sent(),
        BotCommand::AbandonBotCreation {
            request_id: c.form().request()
        }
    );
    let page = BotPage {
        workspace: c.home,
        document: koru_ai_core::DocumentId::new(),
    };
    let mut abandoned = c.creation(BotCreationStage::Abandoned);
    abandoned.role_page_unconfirmed = true;
    abandoned.left_behind = Some(BotCreationLeftovers {
        role_page: Some(page.clone()),
        folder: Some("C:\\koru\\ai\\bots\\b\\work".into()),
    });
    c.reply(abandoned);
    c.view.dialog_closed();
    let lang = c.view.lang;
    let labels: Vec<String> = all_controls(&c.view).into_iter().map(|c| c.label).collect();
    assert!(
        labels
            .iter()
            .any(|l| *l == koru_i18n::fill(t(lang, Words::BotsAbandoned), &["Scout"]))
    );
    assert!(
        labels
            .iter()
            .any(|l| l == t(lang, Words::BotsLeftPageUnconfirmed))
    );
    assert!(labels.iter().any(|l| *l
        == koru_i18n::fill(
            t(lang, Words::BotsLeftFolder),
            &["C:\\koru\\ai\\bots\\b\\work"]
        )));
    assert!(c.view.perform(Act::OpenLeftPage));
    assert_eq!(
        c.view.take_role_request(),
        Some(page),
        "opened as an ordinary page"
    );
    // The draft is kept, under new identities, and can be created again.
    assert!(c.form().editable());
    assert_eq!(c.form().draft().name, "Scout");
    assert!(c.keys_include(CREATE_BUTTON));
}

/// **On completion the tab becomes the Bot's tab on its main conversation**,
/// from the Bot the owner returned — never an identity made up here.
#[test]
fn completion_turns_the_tab_into_the_bots_tab_on_its_main_conversation() {
    let mut c = shown();
    c.fill();
    assert!(c.view.perform(Act::Create));
    let _ = c.sent();
    let made = crate::bots::tests::bot(c.form().bot(), c.home, "Scout", false);
    let mut done = c.creation(BotCreationStage::Completed);
    done.bot = Some(made.clone());
    c.reply(done);
    assert!(!c.view.is_creating());
    assert_eq!(c.view.bot(), Some(made.id));
    assert_eq!(c.view.chat_id(), made.main_chat);
    assert_eq!(c.view.conversation_id(), Some(made.main_chat));
    assert_eq!(c.view.label(), "Scout");
    assert!(c.view.take_created(), "the window lists it");
    assert!(!c.view.take_created(), "once");
    assert!(
        c.view.settings().bot().is_some_and(|bot| bot.id == made.id),
        "its settings hold the owner's Bot"
    );
}

/// **The folder dialog's answer joins the draft, and the keyboard stays on
/// *Choose…***; the managed folder can be chosen again.
#[test]
fn the_folder_dialog_returns_the_keyboard_to_choose() {
    let mut c = shown();
    c.fill();
    c.tab_to(CHOOSE_FOLDER);
    key(&mut c.view, named(winit::keyboard::NamedKey::Enter), None);
    assert_eq!(
        c.view.take_folder_request(),
        Some(None),
        "asked, from nowhere"
    );
    let folder = if cfg!(windows) { "D:\\work" } else { "/work" };
    assert!(c.view.create_folder_chosen(std::path::Path::new(folder)));
    assert_eq!(c.view.focus(), Some(CHOOSE_FOLDER), "the keyboard stayed");
    assert_eq!(c.form().draft().folder.as_deref(), Some(folder));
    assert!(c.keys_include(USE_MANAGED_FOLDER));
    c.nothing_sent();
}

/// **Change access reveals the settings' own access card**: the two choices
/// and the selected pages, with no *Save* of its own.
#[test]
fn change_access_reveals_the_settings_access_card_without_a_save() {
    let mut c = shown();
    c.fill();
    assert!(
        !c.keys_include(region::ACCESS_CHOICE),
        "the default, in words"
    );
    c.tab_to(CHANGE_ACCESS);
    key(&mut c.view, named(winit::keyboard::NamedKey::Enter), None);
    assert_eq!(
        c.view.focus(),
        Some(region::ACCESS_CHOICE),
        "on to the choices"
    );
    assert!(c.keys_include(region::ADD_PAGE));
    assert!(!c.keys_include(region::SAVE_ACCESS), "it goes with Create");
    key(
        &mut c.view,
        named(winit::keyboard::NamedKey::ArrowRight),
        None,
    );
    assert_eq!(c.form().access_shown(), BotAccess::Workspace);
}

/// **An existing page as the role**: chosen through the shared page picker
/// over the home workspace, and sent as `existing` — the page stays the
/// person's. A page of another workspace is not taken.
#[test]
fn an_existing_page_is_sent_as_the_role() {
    let mut c = shown();
    c.fill();
    assert!(
        c.view
            .perform(Act::ChooseRole(crate::bots::creation::RoleKind::Existing))
    );
    assert_eq!(c.form().problem(), Some(Words::BotsRolePageRequired));
    assert!(c.keys_include(CHOOSE_ROLE_PAGE));
    assert!(c.view.perform(Act::ChooseRolePage));
    assert!(c.view.page_picker_open(), "the shared picker");
    let elsewhere = BotPage {
        workspace: WorkspaceId::new(),
        document: koru_ai_core::DocumentId::new(),
    };
    assert!(!c.view.choose_role_page_for_test(elsewhere));
    let page = BotPage {
        workspace: c.home,
        document: koru_ai_core::DocumentId::new(),
    };
    assert!(c.view.choose_role_page_for_test(page.clone()));
    assert!(c.view.perform(Act::Create));
    let BotCommand::CreateBotWithRole { role, .. } = c.sent() else {
        panic!("a create");
    };
    assert_eq!(role, koru_ai_core::bots::BotRoleInput::Existing { page });
}

/// **An inert control is inert to an assistive technology too**: *Create
/// bot* is disabled while the form refuses its input, and a client's click
/// on it sends nothing, as a press would not.
#[test]
fn an_assistive_click_on_a_disabled_control_does_nothing() {
    let mut c = shown();
    assert!(c.form().problem().is_some(), "no name yet");
    assert!(c.keys_include(CREATE_BUTTON));
    assert!(!c.view.accessibility_action(CREATE_BUTTON, true));
    c.nothing_sent();
}

/// **An assistive technology chooses a model row by its key**, through the
/// row's own choose path, not by a press at its published centre — which,
/// for a row below the list's visible part, lands outside the picker.
#[test]
fn an_assistive_click_on_a_model_row_chooses_it_by_its_key() {
    let mut c = shown();
    c.fill();
    assert!(c.view.perform(Act::OpenModelPicker));
    let row = all_controls(&c.view)
        .into_iter()
        .find(|control| control.role == ControlRole::ListItem && control.label == "GPT-5")
        .expect("GPT-5 is published");
    assert_eq!(row.key.part, "row");
    assert!(c.view.accessibility_action(row.key, true));
    assert_eq!(
        c.form().model_shown().unwrap().model.requested_model,
        "gpt-5"
    );
}

/// **Every control the card's popovers publish acts by its key** (the
/// second review's item 11): Fast is toggled, another agent's rail mark
/// selects that agent's list, and a mode row chooses that mode — each
/// through its own choose path, never a press at its published centre.
#[test]
fn an_assistive_click_acts_on_every_popover_control_by_its_key() {
    use koru_ai_ui::agent_catalog::config::picker::elements;
    let mut c = shown();
    c.fill();
    assert!(c.view.perform(Act::OpenModelPicker));
    let gpt = all_controls(&c.view)
        .into_iter()
        .find(|control| control.role == ControlRole::ListItem && control.label == "GPT-5")
        .expect("GPT-5 is published");
    assert!(c.view.accessibility_action(gpt.key, true));
    // Fast, by its key.
    let fast = crate::bots::view::agent::published_key(elements::FAST_KEY);
    assert!(
        all_controls(&c.view)
            .iter()
            .any(|control| control.key == fast),
        "Fast is published for GPT-5"
    );
    assert!(c.view.accessibility_action(fast, true));
    assert_eq!(c.form().model_shown().unwrap().fast, Some(true));
    // Claude's mark, by its key: its list is the one shown.
    let claude_id = AiProviderId::parse("agent-claude").unwrap();
    let claude = elements::mark_key(&RailEntry::Agent(claude_id.clone()));
    assert!(
        all_controls(&c.view)
            .iter()
            .any(|control| control.key == claude),
        "Claude's mark is published"
    );
    assert!(c.view.accessibility_action(claude, true));
    assert_eq!(
        c.view.agent_picker_read(|picker| picker.entry()).flatten(),
        Some(RailEntry::Agent(claude_id))
    );
    c.view.agent = None;
    // A mode row, by its key.
    assert!(c.view.perform(Act::OpenModes));
    let ask = koru_ai_ui::agent_catalog::config::modes::elements::row_key("ask");
    assert!(
        all_controls(&c.view)
            .iter()
            .any(|control| control.key == ask),
        "the ask mode is published"
    );
    assert!(c.view.accessibility_action(ask, true));
    assert_eq!(c.form().model_shown().unwrap().mode.as_deref(), Some("ask"));
}

/// **A model row below the list's visible part is chosen by an assistive
/// click** (review fault 7, its geometry): forty models, so the picker's
/// list scrolls and the last rows are published beneath the part of it a
/// person sees. A click on one through the accessibility tree chooses that
/// model; it never lands as a press outside the picker that closes it.
#[test]
fn an_assistive_click_on_a_model_row_below_the_fold_chooses_it() {
    let mut many = listing();
    many.models = (1..=40)
        .map(|n| CatalogModel {
            id: format!("model-{n}"),
            group: "openai".into(),
            name: Some(format!("Model {n}")),
            description: None,
            group_name: None,
            legacy: false,
            levels: None,
            context: None,
            fast: Some(false),
        })
        .collect();
    let mut c = shown_with("codex", many);
    c.fill();
    assert!(c.view.perform(Act::OpenModelPicker));
    let controls = all_controls(&c.view);
    let list = controls
        .iter()
        .find(|control| {
            control.role == ControlRole::List
                && control.children.iter().any(|row| row.label == "Model 40")
        })
        .expect("the picker's list is published with its rows");
    let row = list
        .children
        .iter()
        .find(|row| row.label == "Model 40")
        .expect("the last model is published")
        .clone();
    assert!(
        row.rect.y >= list.rect.y + list.rect.h,
        "below the fold: row {:?}, list {:?}",
        row.rect,
        list.rect
    );
    assert_eq!(row.key.part, "row");
    assert!(c.view.accessibility_action(row.key, true));
    assert_eq!(
        c.form().model_shown().unwrap().model.requested_model,
        "model-40"
    );
}

/// **Koru could not confirm whether the bot was made** (the third review's
/// fourth finding, through the tab): the Create's answer was lost and the
/// read that followed was refused for good. The form says it could not
/// confirm and to look in Bots, stays locked under the same identities, and
/// offers *Start over*, which alone takes new ones.
#[test]
fn an_unconfirmed_create_offers_start_over_and_keeps_its_identities() {
    let mut c = shown();
    c.fill();
    assert!(c.view.perform(Act::Create));
    c.sent();
    let (request, bot) = (c.form().request(), c.form().bot());
    let reply = |c: &mut Create, result| {
        c.answers
            .send(Answer::Wrote {
                bot,
                request,
                result,
            })
            .expect("the tab's end is open");
        c.view.poll();
    };
    reply(&mut c, Err(Failure::timed_out()));
    c.view.shown_pass(None);
    assert!(matches!(c.sent(), BotCommand::ReadBotCreation { .. }));
    reply(&mut c, Err(Failure::Refused("wrong_owner".into())));
    assert_eq!(c.form().notice(), Some(Words::BotsCreateUnconfirmed));
    assert!(!c.form().editable(), "locked");
    assert_eq!((c.form().request(), c.form().bot()), (request, bot));
    assert!(c.keys_include(START_OVER));
    assert!(!c.view.perform(Act::Create), "no second Create");
    c.nothing_sent();
    assert!(c.view.perform(Act::StartOver));
    assert!(!c.keys_include(START_OVER));
    assert!(c.form().editable());
    assert_ne!(c.form().request(), request, "new identities");
    assert_ne!(c.form().bot(), bot);
    c.nothing_sent();
}
