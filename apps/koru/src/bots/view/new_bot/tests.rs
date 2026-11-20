// SPDX-License-Identifier: MPL-2.0

//! **The *New bot* dialog**, driven through a link and a catalog link the
//! test answers: what it draws, what its keys do, and that its Create is
//! the create form's own command, replayed and never doubled.

use std::sync::mpsc::{Receiver, SyncSender};

use koru_ai_core::bots::{
    BotCommand, BotCreation, BotCreationPhase, BotCreationReason, BotCreationStage,
    BotFolderChoice, BotResponse, BotRoleInput,
};
use koru_ai_ui::agent_catalog::config::picker::Readiness;
use koru_ai_ui::agent_catalog::{CatalogAgent, CatalogListing, CatalogMode, CatalogModel};

use super::*;
use crate::bots::catalog::{self, Catalog, CatalogLink};
use crate::bots::link::{Answer, BotsLink, Command, Failure};

/// A channel root that is only ever joined to, never opened.
const CHANNEL_ROOT: &str = "C:\\koru-test-root";

fn agent_key(index: usize) -> Key {
    super::agent_key(&AiProviderId::parse(["agent-claude", "agent-codex"][index]).unwrap())
}

struct Fx {
    dialog: NewBotDialog,
    commands: Receiver<Command>,
    answers: SyncSender<Answer>,
    home: WorkspaceId,
    catalog_commands: Receiver<catalog::Command>,
    catalog_answers: SyncSender<catalog::Answer>,
    settings: AgentSettings,
}

fn agent(id: &str, label: &str) -> CatalogAgent {
    CatalogAgent {
        id: AiProviderId::parse(id).expect("a provider id"),
        key: Some(id.trim_start_matches("agent-").to_owned()),
        label: label.into(),
        mark: koru_draw::brand::ProfileMark::Glyph(koru_draw::icons::ROBOT),
    }
}

fn modes(ids: &[&str], default: &str) -> CatalogListing {
    CatalogListing {
        models: vec![CatalogModel {
            id: "model".into(),
            group: "group".into(),
            name: Some("Model".into()),
            description: None,
            group_name: None,
            legacy: false,
            levels: None,
            context: None,
            fast: Some(false),
        }],
        modes: ids
            .iter()
            .map(|id| CatalogMode {
                id: (*id).into(),
                name: None,
                description: None,
            })
            .collect(),
        default_mode: Some(default.into()),
    }
}

/// What each agent lists: its modes as the real agents name them.
fn listing_of(agent: &AiProviderId) -> CatalogListing {
    if agent.as_str() == "agent-claude" {
        modes(
            &[
                "default",
                "acceptEdits",
                "plan",
                "auto",
                "bypassPermissions",
            ],
            "default",
        )
    } else {
        modes(&["ask", "auto-review", "full-access"], "ask")
    }
}

/// **A standing dialog** whose catalog answered with Claude then Codex in
/// the person's order, both ready, and Codex the person's default agent for
/// new chats: the dialog must start on the first of the list all the same.
fn open() -> Fx {
    let (link, commands, answers) = BotsLink::test_pair();
    let home = WorkspaceId::new();
    let mut host = BotView::creating_with_link(home, link);
    let (catalog_link, catalog_commands, catalog_answers) = CatalogLink::test_pair();
    host.catalog = Box::new(Catalog::with_link(catalog_link));
    host.catalog.shown();
    let settings = AgentSettings {
        order: vec!["claude".into(), "codex".into()],
        default_agent: Some("codex".into()),
        ..Default::default()
    };
    let dialog = NewBotDialog::over(host, Some(PathBuf::from(CHANNEL_ROOT)), (1200.0, 800.0));
    let mut fx = Fx {
        dialog,
        commands,
        answers,
        home,
        catalog_commands,
        catalog_answers,
        settings,
    };
    fx.pump();
    fx
}

impl Fx {
    /// Run the dialog's pass, answering every catalog read it asks for.
    fn pump(&mut self) {
        for _ in 0..5 {
            self.dialog.pass(None, &self.settings);
            while let Ok(command) = self.catalog_commands.try_recv() {
                let answer = match command {
                    catalog::Command::Agents { seq } => catalog::Answer::Agents {
                        seq,
                        result: Ok(vec![
                            agent("agent-codex", "Codex"),
                            agent("agent-claude", "Claude"),
                        ]),
                    },
                    catalog::Command::Readiness { seq, agent } => catalog::Answer::Readiness {
                        seq,
                        result: Ok(Readiness::Ready),
                        agent,
                    },
                    catalog::Command::Listing { seq, agent, .. } => catalog::Answer::Listing {
                        seq,
                        result: Ok(listing_of(&agent)),
                        agent,
                    },
                };
                self.catalog_answers
                    .send(answer)
                    .expect("the catalog's end");
            }
        }
        self.dialog.pass(None, &self.settings);
        self.well_formed();
    }

    /// The frame is one an assistive technology can take: no key twice, no
    /// node unnamed or unkeyed.
    fn well_formed(&self) {
        self.dialog.with_frame(|frame, _| {
            assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
        });
    }

    fn key(&mut self, key: NamedKey) -> Outcome {
        let outcome = self
            .dialog
            .key(&WinitKey::Named(key), None, ModifiersState::empty());
        self.well_formed();
        outcome
    }

    fn shift_tab(&mut self) -> Outcome {
        self.dialog
            .key(&WinitKey::Named(NamedKey::Tab), None, ModifiersState::SHIFT)
    }

    fn has(&self, key: Key) -> bool {
        self.dialog.with_frame(|frame, _| frame.contains(key))
    }

    fn disabled(&self, key: Key) -> bool {
        self.dialog
            .with_frame(|frame, _| frame.node(key).expect("drawn").disabled)
    }

    fn selected(&self, key: Key) -> Option<bool> {
        self.dialog
            .with_frame(|frame, _| frame.node(key).expect("drawn").selected)
    }

    fn centre(&self, key: Key) -> (f32, f32) {
        self.dialog.with_frame(|frame, _| {
            let rect = frame.node(key).expect("drawn").rect;
            (rect.x + rect.w * 0.5, rect.y + rect.h * 0.5)
        })
    }

    fn press(&mut self, key: Key) -> Outcome {
        let (x, y) = self.centre(key);
        let outcome = self.dialog.mouse_down(x, y);
        self.dialog.mouse_up();
        self.well_formed();
        outcome
    }

    /// Reach a folder action through the same scroll path as the
    /// wheel, then verify its visible center belongs to that action.
    fn scroll_to_action(&mut self, key: Key) {
        let extent = self
            .dialog
            .with_frame(|frame, _| frame.node(BODY).and_then(|node| node.scroll).unwrap().0);
        self.dialog.scroll_by(extent);
        let (x, y) = self.centre(key);
        self.dialog.with_frame(|frame, _| {
            assert_eq!(
                frame.hit_at(x, y),
                Some(key),
                "the action is visible and clickable"
            );
        });
        self.well_formed();
    }

    /// The one creation command the dialog just sent, by its draft.
    fn sent(&self) -> BotCommand {
        let form = self.dialog.form().expect("a draft");
        match self.commands.try_recv().expect("a command was sent") {
            Command::Write {
                bot,
                request,
                command,
            } => {
                assert_eq!(bot, form.bot(), "by the BotId the draft reserves");
                assert_eq!(request, form.request(), "by the draft's request");
                *command
            }
            _ => panic!("a creation command"),
        }
    }

    fn nothing_sent(&self) {
        assert!(self.commands.try_recv().is_err(), "nothing was sent");
    }

    /// Answer the draft's last write as the owner would.
    fn answer(&mut self, result: Result<BotResponse, Failure>) {
        let form = self.dialog.form().expect("a draft");
        self.answers
            .send(Answer::Wrote {
                bot: form.bot(),
                request: form.request(),
                result,
            })
            .expect("the dialog's end is open");
        self.dialog.pass(None, &self.settings);
        if !self.dialog.created() {
            self.well_formed();
        }
    }

    fn creation(&self, stage: BotCreationStage) -> BotCreation {
        let form = self.dialog.form().expect("a draft");
        BotCreation {
            request_id: form.request(),
            bot_id: form.bot(),
            home_workspace: self.home,
            name: form.draft().name.trim().to_owned(),
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
}

/// **Open: the keyboard is in the name field, the first face is picked,
/// the preview wears no initial yet, and Create cannot be pressed.** A name
/// of spaces is no name; the first real character enables Create and is the
/// preview's initial.
#[test]
fn create_is_disabled_until_the_name_is_more_than_spaces() {
    let mut fx = open();
    assert_eq!(fx.dialog.focus(), NAME, "focused on open");
    assert_eq!(fx.dialog.preview(), (0, String::new()));
    assert!(fx.disabled(CREATE), "no name yet");
    assert!(!fx.dialog.can_create());
    // Enter with nothing to create sends nothing and closes nothing.
    assert_eq!(fx.key(NamedKey::Enter), Outcome::Ignored);
    fx.nothing_sent();

    fx.dialog.type_name("   ");
    assert_eq!(fx.dialog.name(), "   ");
    assert!(fx.disabled(CREATE), "spaces are not a name");
    assert_eq!(fx.key(NamedKey::Enter), Outcome::Ignored);
    fx.nothing_sent();
    assert!(
        !fx.has(PROBLEM),
        "the empty name is not said as a refusal: Create is just not offered"
    );

    fx.dialog.type_name("scout");
    assert_eq!(fx.dialog.name(), "   scout");
    assert!(!fx.disabled(CREATE), "named");
    assert!(fx.dialog.can_create());
    assert_eq!(fx.dialog.preview(), (0, "S".to_owned()), "the initial");
    fx.well_formed();
}

/// **Picking a face and *Show me others* move the preview and the request.**
/// The grid shows twelve faces, the picked one pressed; *Show me others*
/// shows the other twelve and changes neither the pick nor the preview; a
/// face picked there is what Create sends.
#[test]
fn picking_a_face_and_show_me_others_update_the_preview_and_the_request() {
    let mut fx = open();
    fx.dialog.type_name("Scout");
    let shown = |fx: &Fx| -> Vec<u8> {
        (0..FACE_COUNT)
            .filter(|face| fx.has(face_key(*face)))
            .collect()
    };
    assert_eq!(shown(&fx), (0..12).collect::<Vec<_>>(), "the first twelve");
    assert_eq!(fx.selected(face_key(0)), Some(true), "the first is picked");

    assert_eq!(fx.press(face_key(4)), Outcome::Changed);
    assert_eq!(fx.dialog.preview(), (4, "S".to_owned()));
    assert_eq!(fx.selected(face_key(4)), Some(true));
    assert_eq!(fx.selected(face_key(0)), Some(false), "one at a time");

    assert_eq!(fx.press(OTHERS), Outcome::Changed);
    assert_eq!(fx.dialog.set(), 1);
    assert_eq!(shown(&fx), (12..24).collect::<Vec<_>>(), "the other twelve");
    assert_eq!(fx.dialog.preview(), (4, "S".to_owned()), "the pick stays");
    assert!(
        (12..24).all(|face| fx.selected(face_key(face)) == Some(false)),
        "none of these is the pick"
    );

    assert_eq!(fx.press(face_key(17)), Outcome::Changed);
    assert_eq!(fx.dialog.preview(), (17, "S".to_owned()));
    assert_eq!(fx.press(OTHERS), Outcome::Changed, "and back");
    assert_eq!(fx.dialog.set(), 0);
    assert_eq!(fx.dialog.preview(), (17, "S".to_owned()));

    assert_eq!(fx.press(CREATE), Outcome::Changed);
    let BotCommand::CreateBotWithRole { config, .. } = fx.sent() else {
        panic!("a create");
    };
    assert_eq!(config.avatar, Some(17), "the face picked is the face sent");
}

/// **One Create, by the form's own command**: the name trimmed, the face,
/// the first agent of the person's list at its *Default* model (not their
/// default agent for chats), the agent's automatic mode, an empty authored
/// Role titled by the name, the approved access default and the managed
/// folder. **A second press sends nothing**; **a Create whose answer is
/// lost is replayed** under the same request and BotId with the same body,
/// which is what lets the owner answer it once; and when the owner says the
/// Bot is made, the dialog's host is that Bot's tab.
#[test]
fn create_carries_the_defaults_and_replays_under_one_request() {
    let mut fx = open();
    fx.dialog.type_name("  Scout ");
    assert_eq!(fx.press(face_key(7)), Outcome::Changed);
    fx.dialog.set_focus(NAME);
    assert_eq!(fx.key(NamedKey::Enter), Outcome::Changed, "Enter creates");
    let first = fx.sent();
    let BotCommand::CreateBotWithRole {
        request_id,
        bot_id,
        home_workspace,
        role,
        folder,
        config,
        access,
        selected_pages,
    } = first.clone()
    else {
        panic!("a create");
    };
    assert_eq!(home_workspace, fx.home);
    assert_eq!(config.name, "Scout", "trimmed");
    assert_eq!(config.avatar, Some(7));
    assert_eq!(
        config.model.provider.as_str(),
        "agent-claude",
        "the first agent of the person's list, not their default for chats"
    );
    assert_eq!(config.model.requested_model, "default");
    assert_eq!(
        config.mode.as_deref(),
        Some("auto"),
        "the agent's automatic mode, never its asking one nor full access"
    );
    assert_eq!(config.fast, None);
    assert_eq!(
        folder,
        BotFolderChoice::Managed {},
        "the owner's own folder"
    );
    assert_eq!(
        role,
        BotRoleInput::Authored {
            title: "Scout".into(),
            markdown: String::new(),
        }
    );
    assert_eq!(access, koru_ai_core::bots::BotAccess::Default);
    assert!(selected_pages.is_empty());
    let form = fx.dialog.form().unwrap();
    assert_eq!((request_id, bot_id), (form.request(), form.bot()));

    // On its way: *Creating…*, the inputs locked, and a second press sends
    // nothing.
    assert!(fx.has(CREATING));
    assert!(fx.disabled(NAME) && fx.disabled(face_key(3)));
    assert_eq!(fx.dialog.activate(CREATE), Outcome::Ignored);
    assert_eq!(fx.key(NamedKey::Enter), Outcome::Ignored);
    fx.nothing_sent();

    // The answer is lost: the owner may hold it, so the dialog asks what it
    // holds, by the same request, before anything can change.
    fx.answer(Err(Failure::lost()));
    assert!(fx.dialog.held());
    assert_eq!(
        fx.sent(),
        BotCommand::ReadBotCreation { request_id },
        "read, not created again"
    );
    // The owner never took it: the draft is the person's again, under the
    // same identities, and Create sends the very same command.
    fx.answer(Err(Failure::Refused("not_found".into())));
    assert!(!fx.dialog.held());
    assert!(!fx.disabled(CREATE));
    assert_eq!(fx.dialog.activate(CREATE), Outcome::Changed);
    assert_eq!(
        fx.sent(),
        first,
        "the same request, BotId and body: a replay"
    );

    // Made: the host is the new Bot's tab, on its main conversation.
    assert!(!fx.dialog.created());
    let made = crate::bots::tests::bot(bot_id, fx.home, "Scout", false);
    let main_chat = made.main_chat;
    let mut done = fx.creation(BotCreationStage::Completed);
    done.bot = Some(made);
    fx.answer(Ok(BotResponse::Creation { creation: done }));
    assert!(fx.dialog.created(), "the dialog closes");
    fx.nothing_sent();
    let tab = fx.dialog.into_tab();
    assert_eq!(tab.bot(), Some(bot_id));
    assert_eq!(tab.conversation_id(), Some(main_chat), "its chat opens");
    assert!(!tab.is_creating());
}

/// **The folder is shown before the Bot exists, and can be changed.** The
/// one the dialog names is the path the owner makes for a managed folder,
/// worked out with no disk access; *Change…* asks the window for its folder
/// dialog and a chosen folder is what Create sends; the managed one can be
/// taken back.
#[test]
fn the_folder_is_shown_and_a_chosen_one_is_sent() {
    let mut fx = open();
    fx.dialog.type_name("Scout");
    let bot = fx.dialog.draft_bot().expect("a draft");
    let managed = first_bot::default_folder(std::path::Path::new(CHANNEL_ROOT), bot);
    let managed = managed.to_string_lossy().into_owned();
    assert_eq!(fx.dialog.folder_path().as_deref(), Some(managed.as_str()));
    assert!(
        managed.starts_with(CHANNEL_ROOT),
        "under Koru's local data: {managed}"
    );
    assert!(
        managed.contains(&bot.to_storage()),
        "the Bot's own folder: {managed}"
    );
    // The folder and chooser are visible without a disclosure.
    let summary = fx.dialog.summary();
    assert!(summary.contains(&managed), "{summary}");
    assert!(fx.has(CHANGE_FOLDER) && fx.has(FOLDER));
    assert!(!fx.has(MANAGED_FOLDER), "nothing chosen to take back");
    fx.scroll_to_action(CHANGE_FOLDER);
    assert_eq!(fx.press(CHANGE_FOLDER), Outcome::Changed);
    assert_eq!(
        fx.dialog.take_folder_request(),
        Some(None),
        "the window's folder dialog, starting nowhere in particular"
    );
    assert_eq!(fx.dialog.take_folder_request(), None, "once");
    fx.nothing_sent();

    let chosen = std::path::Path::new("D:\\work\\scout");
    assert!(fx.dialog.folder_chosen(chosen));
    assert_eq!(fx.dialog.folder_path().as_deref(), Some("D:\\work\\scout"));
    assert!(fx.has(MANAGED_FOLDER));
    assert_eq!(fx.dialog.activate(CREATE), Outcome::Changed);
    let BotCommand::CreateBotWithRole { folder, .. } = fx.sent() else {
        panic!("a create");
    };
    assert_eq!(
        folder,
        BotFolderChoice::Selected {
            path: "D:\\work\\scout".into()
        }
    );
}

/// The managed folder is taken back with one press, before anything is
/// sent.
#[test]
fn the_managed_folder_can_be_taken_back() {
    let mut fx = open();
    fx.dialog.type_name("Scout");
    assert!(
        fx.dialog
            .folder_chosen(std::path::Path::new("D:\\work\\scout"))
    );
    assert_eq!(fx.press(MANAGED_FOLDER), Outcome::Changed);
    assert!(!fx.has(MANAGED_FOLDER));
    assert_eq!(fx.dialog.activate(CREATE), Outcome::Changed);
    let BotCommand::CreateBotWithRole { folder, .. } = fx.sent() else {
        panic!("a create");
    };
    assert_eq!(folder, BotFolderChoice::Managed {});
}

/// **The agent is shown and can be switched directly**:
/// every agent the person has not turned off is offered in their order,
/// the first chosen; choosing another sends it, at its own automatic mode.
#[test]
fn the_agent_is_the_first_of_the_list_and_can_be_switched() {
    let mut fx = open();
    fx.dialog.type_name("Scout");
    assert!(fx.has(AGENTS));
    assert!(fx.has(MANAGE_AGENTS));
    let (claude, codex) = (agent_key(0), agent_key(1));
    assert_eq!(fx.selected(claude), Some(true), "the first of the list");
    assert_eq!(fx.selected(codex), Some(false));
    let names = fx.dialog.with_frame(|frame, _| {
        [claude, codex].map(|key| match &frame.node(key).unwrap().label {
            Some(koru_draw::el::Name::Text(text)) => text.clone(),
            other => panic!("an agent is named by its own name: {other:?}"),
        })
    });
    assert_eq!(names, ["Claude".to_owned(), "Codex".to_owned()]);

    assert_eq!(fx.press(codex), Outcome::Changed);
    fx.pump();
    assert_eq!(fx.selected(codex), Some(true));
    assert_eq!(fx.dialog.host.agent_label(), "Codex");
    assert_eq!(fx.dialog.activate(CREATE), Outcome::Changed);
    let BotCommand::CreateBotWithRole { config, .. } = fx.sent() else {
        panic!("a create");
    };
    assert_eq!(config.model.provider.as_str(), "agent-codex");
    assert_eq!(
        config.mode.as_deref(),
        Some("auto-review"),
        "Codex's automatic mode"
    );
}

#[test]
fn runtime_setup_keeps_the_draft_and_sends_nothing() {
    let mut fx = open();
    fx.dialog.type_name("Grok bot");
    fx.dialog.pick(5);
    fx.press(agent_key(1));
    let bot = fx.dialog.draft_bot();
    fx.dialog.set_focus(MANAGE_AGENTS);
    assert_eq!(fx.key(NamedKey::Enter), Outcome::ManageAgents);
    fx.nothing_sent();
    let tab = fx.dialog.into_setup_tab();
    let create = tab.create.as_ref().unwrap();
    assert_eq!(Some(create.form.bot()), bot);
    assert_eq!(create.form.draft().name, "Grok bot");
    assert_eq!(create.form.draft().avatar, Some(5));
    assert_eq!(tab.agent_provider().unwrap().as_str(), "agent-codex");
    assert!(
        create.first_bot,
        "a setup visit preserves the dialog's defaults"
    );
}

#[test]
fn an_agent_create_refusal_has_one_message_and_a_settings_link() {
    let mut fx = open();
    fx.dialog.type_name("Scout");
    assert_eq!(fx.dialog.activate(CREATE), Outcome::Changed);
    fx.sent();
    let form = fx.dialog.form().unwrap();
    fx.answers
        .send(Answer::Reason {
            bot: form.bot(),
            request: form.request(),
            reason: "agent_failed_to_start".into(),
        })
        .unwrap();
    fx.answer(Err(Failure::Refused("agent_models_failed".into())));
    assert!(fx.has(AGENT_READINESS));
    assert!(
        !fx.has(NOTICE),
        "the same refusal must not appear in the footer"
    );
    assert_eq!(
        fx.dialog.agent_problem().as_deref(),
        Some(
            "Claude didn't start on this computer. Check that it opens on its own, then try again."
        )
    );
    fx.dialog.set_focus(AGENT_SETTINGS);
    assert_eq!(fx.key(NamedKey::Enter), Outcome::ManageAgents);
    fx.nothing_sent();
    assert_eq!(
        fx.dialog.into_setup_tab().create.unwrap().form.draft().name,
        "Scout"
    );
}

#[test]
fn provider_management_and_the_folder_fit_the_compact_dialog() {
    let mut fx = open();
    for width in [360.0, 1200.0] {
        fx.dialog.set_viewport((width, 1000.0));
        fx.dialog.with_frame(|frame, _| {
            let label = frame.named("New bot agents label").next().unwrap().rect;
            let manage = frame.node(MANAGE_AGENTS).unwrap().rect;
            assert!((label.y + label.h / 2.0 - manage.y - manage.h / 2.0).abs() < 0.01);
            assert!(manage.x >= label.x + label.w);
            let group = frame.node(AGENTS).unwrap().rect;
            assert!(manage.x + manage.w <= group.x + group.w + 0.01);
            let folder = frame.node(FOLDER).unwrap();
            assert_eq!(folder.lines(), Some(1));
            assert_eq!(
                folder.label,
                Some(koru_draw::el::Name::Text(fx.dialog.summary()))
            );
            assert!(folder.rect.x + folder.rect.w <= group.x + group.w + 0.01);
        });
    }
}

#[test]
fn runtime_choices_include_the_whole_catalog_and_wrap_inside_the_dialog() {
    let mut fx = open();
    let (link, commands, answers) = CatalogLink::test_pair();
    fx.dialog.host.catalog = Box::new(Catalog::with_link(link));
    fx.dialog.host.catalog.shown();
    fx.dialog.pass(None, &fx.settings);
    let catalog::Command::Agents { seq } = commands.try_recv().unwrap() else {
        panic!("the agent catalog is requested first");
    };
    let agents: Vec<_> = (0..11)
        .map(|i| agent(&format!("agent-test-{i}"), &format!("Runtime {i}")))
        .collect();
    answers
        .send(catalog::Answer::Agents {
            seq,
            result: Ok(agents.clone()),
        })
        .unwrap();
    fx.dialog.pass(None, &fx.settings);
    assert_eq!(fx.dialog.agents().len(), 11);
    for width in [360.0, 1200.0] {
        fx.dialog.set_viewport((width, 1000.0));
        fx.dialog.with_frame(|frame, _| {
            assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
            let group = frame.node(AGENTS).unwrap().rect;
            let mut tops = Vec::new();
            for agent in &agents {
                let rect = frame.node(super::agent_key(&agent.id)).unwrap().rect;
                assert!(rect.x >= group.x && rect.x + rect.w <= group.x + group.w + 0.5);
                assert!(rect.h > 0.0);
                tops.push(rect.y);
            }
            assert!(tops.last().unwrap() > &tops[0], "choices wrap into rows");
        });
    }
    assert!(
        fx.dialog.choose_agent(10),
        "the last runtime remains selectable"
    );
    assert_eq!(fx.dialog.host.agent_provider().unwrap(), agents[10].id);
    fx.nothing_sent();
}

/// **Cloud is there and cannot be chosen**: drawn, named as not available
/// yet, inert to the pointer, off the keyboard's walk, and never selected.
/// *This device* is the choice, and the words under the track say where
/// the Bot runs and why Cloud cannot be picked.
#[test]
fn cloud_is_drawn_and_cannot_be_selected() {
    let mut fx = open();
    fx.dialog.type_name("Scout");
    assert!(fx.has(CLOUD) && fx.has(THIS_DEVICE));
    assert!(fx.disabled(CLOUD));
    assert_eq!(fx.selected(THIS_DEVICE), Some(true));
    assert_eq!(fx.selected(CLOUD), Some(false));
    assert!(!fx.dialog.tab_stops().contains(&CLOUD));
    assert!(
        fx.dialog
            .with_frame(|frame, _| frame.hits().iter().all(|hit| hit.key != CLOUD)),
        "not a pointer target"
    );
    let focus = fx.dialog.focus();
    assert_eq!(fx.press(CLOUD), Outcome::Ignored);
    assert_eq!(fx.dialog.activate(CLOUD), Outcome::Ignored);
    assert_eq!(
        fx.dialog.accessibility_action(CLOUD, true),
        Outcome::Ignored
    );
    assert_eq!(fx.dialog.focus(), focus, "the keyboard did not go there");
    assert_eq!(fx.selected(THIS_DEVICE), Some(true), "still this device");
    assert_eq!(fx.selected(CLOUD), Some(false));
    // In words, not by its dimmer ink alone.
    let label = fx
        .dialog
        .with_frame(|frame, _| frame.node(CLOUD).unwrap().label.clone());
    assert_eq!(
        label,
        Some(koru_draw::el::Name::Key(Words::BotsNewBotCloudUnavailable))
    );
    assert!(fx.has(LOCAL_NOTE) && fx.has(CLOUD_NOTE));
    // And the request says nothing of a place: every Bot runs on its owner.
    assert_eq!(fx.dialog.activate(CREATE), Outcome::Changed);
    assert!(matches!(fx.sent(), BotCommand::CreateBotWithRole { .. }));
}

/// **Escape, Cancel and Close create nothing**: with a name typed and a
/// face picked, each asks the window to drop the dialog, and no command
/// was ever sent.
#[test]
fn escape_cancel_and_close_create_nothing() {
    for close in ["escape", "cancel", "close"] {
        let mut fx = open();
        fx.dialog.type_name("Scout");
        fx.press(face_key(3));
        let outcome = match close {
            "escape" => fx.key(NamedKey::Escape),
            "cancel" => fx.press(CANCEL),
            _ => fx.press(CLOSE),
        };
        assert_eq!(outcome, Outcome::Cancel, "{close}");
        assert!(!fx.dialog.held() && !fx.dialog.created(), "{close}");
        fx.nothing_sent();
    }
    // Enter on Cancel presses Cancel: it does not create.
    let mut fx = open();
    fx.dialog.type_name("Scout");
    fx.dialog.set_focus(CANCEL);
    assert_eq!(fx.key(NamedKey::Enter), Outcome::Cancel);
    fx.nothing_sent();
}

/// **Tab follows the order the controls are drawn in**, the grid as one
/// stop, and wraps; Shift+Tab goes back. Create joins the walk once there
/// is a name; folder controls are always available.
#[test]
fn tab_follows_the_visual_order() {
    let mut fx = open();
    assert_eq!(
        fx.dialog.tab_stops(),
        [
            CLOSE,
            NAME,
            face_key(0),
            OTHERS,
            THIS_DEVICE,
            MANAGE_AGENTS,
            agent_key(0),
            CHANGE_FOLDER,
            FULL_FORM,
            CANCEL
        ],
        "Create is not a stop while it cannot act"
    );
    fx.dialog.type_name("Scout");
    assert_eq!(
        fx.dialog.tab_stops(),
        [
            CLOSE,
            NAME,
            face_key(0),
            OTHERS,
            THIS_DEVICE,
            MANAGE_AGENTS,
            agent_key(0),
            CHANGE_FOLDER,
            FULL_FORM,
            CANCEL,
            CREATE
        ]
    );
    // From the name field, forward through every stop and round again.
    let mut walked = vec![fx.dialog.focus()];
    for _ in 0..11 {
        assert_eq!(fx.key(NamedKey::Tab), Outcome::Changed);
        walked.push(fx.dialog.focus());
    }
    assert_eq!(
        walked,
        [
            NAME,
            face_key(0),
            OTHERS,
            THIS_DEVICE,
            MANAGE_AGENTS,
            agent_key(0),
            CHANGE_FOLDER,
            FULL_FORM,
            CANCEL,
            CREATE,
            CLOSE,
            NAME
        ]
    );
    assert_eq!(fx.shift_tab(), Outcome::Changed);
    assert_eq!(fx.dialog.focus(), CLOSE, "and back");
}

/// **The arrows move in the grid and pick as they move**; Home and End go
/// to either end of the twelve shown; the edges stop. On the other twelve
/// the grid's stop is the first of them, and the arrows pick from there.
/// Enter on a face creates: it is not a button to press.
#[test]
fn the_arrows_move_and_pick_in_the_grid() {
    let mut fx = open();
    fx.dialog.type_name("Scout");
    fx.key(NamedKey::Tab);
    assert_eq!(fx.dialog.focus(), face_key(0));
    for (key, face) in [
        (NamedKey::ArrowRight, 1),
        (NamedKey::ArrowDown, 7),
        (NamedKey::ArrowDown, 7),
        (NamedKey::ArrowLeft, 6),
        (NamedKey::ArrowUp, 0),
        (NamedKey::ArrowUp, 0),
        (NamedKey::ArrowLeft, 0),
        (NamedKey::End, 11),
        (NamedKey::ArrowRight, 11),
        (NamedKey::Home, 0),
        (NamedKey::ArrowRight, 1),
    ] {
        assert_eq!(fx.key(key), Outcome::Changed, "{key:?}");
        assert_eq!(fx.dialog.focus(), face_key(face), "{key:?}");
        assert_eq!(fx.dialog.face(), face, "the pick follows the keyboard");
    }
    assert_eq!(
        fx.dialog.tab_stops()[2],
        face_key(1),
        "the grid's stop is the picked face"
    );

    fx.dialog.activate(OTHERS);
    assert_eq!(
        fx.dialog.tab_stops()[2],
        face_key(12),
        "the pick is not among these: the first of them"
    );
    fx.dialog.set_focus(face_key(12));
    assert_eq!(fx.key(NamedKey::ArrowRight), Outcome::Changed);
    assert_eq!((fx.dialog.focus(), fx.dialog.face()), (face_key(13), 13));

    assert_eq!(fx.key(NamedKey::Enter), Outcome::Changed, "Enter creates");
    let BotCommand::CreateBotWithRole { config, .. } = fx.sent() else {
        panic!("a create");
    };
    assert_eq!(config.avatar, Some(13));
}

/// **A creation that stops unfinished keeps the dialog up, in the form's
/// own words**: why it stopped, what Retry can do, and Retry. Closing the
/// dialog then does not drop the creation: it asks for the create tab on
/// the same draft, where Abandon is. A refusal before anything was made is
/// said by its code's sentence and leaves the draft the person's.
#[test]
fn a_failure_keeps_the_dialog_open_with_the_forms_own_words() {
    let mut fx = open();
    fx.dialog.type_name("Scout");
    fx.dialog.set_focus(CREATE);
    assert_eq!(fx.key(NamedKey::Enter), Outcome::Changed);
    let BotCommand::CreateBotWithRole { request_id, .. } = fx.sent() else {
        panic!("a create");
    };
    let mut stopped = fx.creation(BotCreationStage::Incomplete);
    stopped.reason = Some(BotCreationReason::FolderUnavailable);
    stopped.retryable = true;
    fx.answer(Ok(BotResponse::Creation { creation: stopped }));
    assert!(!fx.dialog.created());
    assert!(fx.dialog.held(), "the owner holds it");
    let lang = fx.dialog.lang;
    let said = |fx: &Fx, key: Key| {
        fx.dialog
            .with_frame(|frame, _| match &frame.node(key)?.label {
                Some(koru_draw::el::Name::Text(text)) => Some(text.clone()),
                _ => None,
            })
    };
    assert_eq!(
        said(&fx, STAGE_PARTS[0]).as_deref(),
        Some(t(lang, Words::BotsCreateFolderUnavailable)),
        "why it stopped"
    );
    assert_eq!(
        said(&fx, STAGE_PARTS[1]).as_deref(),
        Some(t(lang, Words::BotsCreateIncomplete)),
        "and what can be done"
    );
    assert!(fx.has(RETRY) && !fx.has(CREATE), "Retry in Create's place");
    assert!(
        fx.dialog.tab_stops().contains(&fx.dialog.focus()),
        "the keyboard is on a control that is there"
    );
    assert_eq!(fx.press(RETRY), Outcome::Changed);
    assert_eq!(
        fx.sent(),
        BotCommand::RetryBotCreation { request_id },
        "by the same request"
    );
    // Closing it now keeps the creation: the create tab, not nothing.
    assert_eq!(fx.key(NamedKey::Escape), Outcome::OpenForm);
    assert_eq!(fx.dialog.dismiss(), Outcome::OpenForm);
    let tab = fx.dialog.into_tab();
    assert!(tab.is_creating());
    assert_eq!(
        tab.create_form().map(|form| form.request()),
        Some(request_id),
        "the same draft, in the strip"
    );

    // A refusal before anything was made.
    let mut fx = open();
    fx.dialog.type_name("Scout");
    assert_eq!(fx.dialog.activate(CREATE), Outcome::Changed);
    let _ = fx.sent();
    fx.answer(Err(Failure::Refused("invalid_request".into())));
    assert!(!fx.dialog.created() && !fx.dialog.held());
    assert!(fx.has(NOTICE), "said beside the buttons");
    assert!(!fx.disabled(CREATE), "and the person may try again");
}

/// ***Open the full form* keeps the draft**: the name typed and the face
/// picked go to the create tab, which then starts a Bot by its own rule
/// (the agent's asking mode), and nothing was sent by asking.
#[test]
fn the_full_form_takes_the_draft_and_its_own_rules() {
    let mut fx = open();
    fx.dialog.type_name("Scout");
    fx.press(face_key(9));
    fx.scroll_to_action(FULL_FORM);
    assert_eq!(fx.press(FULL_FORM), Outcome::OpenForm);
    fx.nothing_sent();
    let tab = fx.dialog.into_tab();
    assert!(tab.is_creating());
    let form = tab.create_form().expect("the create form");
    assert_eq!(form.draft().name, "Scout");
    assert_eq!(form.draft().avatar, Some(9));
    assert_eq!(
        tab.asking_mode().as_deref(),
        Some("default"),
        "the form's rule: the agent's asking mode"
    );
}

/// **Every control is named and placed, and the panel fits the window.**
/// At the design's size the panel is the design's width, in the middle; in
/// a window too small for it the panel is what the window leaves, the body
/// scrolls, and the footer's buttons are still inside the window.
#[test]
fn the_panel_fits_the_window_it_is_in() {
    let mut fx = open();
    fx.dialog.type_name("Scout");
    let panel = fx.dialog.with_frame(|_, panel| panel);
    assert_eq!(panel.w, DIALOG_W.value);
    assert!((panel.x + panel.w * 0.5 - 600.0).abs() < 0.51, "{panel:?}");
    assert!(panel.y >= 0.0 && panel.y + panel.h <= 800.0, "{panel:?}");
    let scrolls = |fx: &Fx| {
        fx.dialog.with_frame(|frame, _| {
            let body = frame.node(BODY).unwrap();
            body.scroll
                .is_some_and(|(extent, _)| extent > body.rect.h + 0.5)
        })
    };
    assert!(!scrolls(&fx), "everything shows at this size");

    fx.dialog.set_viewport((320.0, 300.0));
    fx.well_formed();
    let panel = fx.dialog.with_frame(|_, panel| panel);
    assert!(panel.w <= 320.0 && panel.h <= 300.0, "{panel:?}");
    assert!(panel.x >= 0.0 && panel.y >= 0.0, "{panel:?}");
    assert!(scrolls(&fx), "the body scrolls rather than hiding rows");
    let create = fx
        .dialog
        .with_frame(|frame, _| frame.node(CREATE).unwrap().rect);
    assert!(
        create.y >= panel.y && create.y + create.h <= panel.y + panel.h,
        "Create stays inside the panel: {create:?} in {panel:?}"
    );
    // The keyboard brings a control under the fold into view.
    fx.dialog.set_focus(CHANGE_FOLDER);
    let (more, body) = fx.dialog.with_frame(|frame, _| {
        (
            frame.node(CHANGE_FOLDER).unwrap().rect,
            frame.node(BODY).unwrap().rect,
        )
    });
    assert!(
        more.y >= body.y - 0.5 && more.y + more.h <= body.y + body.h + 0.5,
        "{more:?} in {body:?}"
    );
}

/// **A long name and a name in another script** are a name: Create is
/// offered, the preview wears the first letter, and the frame stays whole.
#[test]
fn long_and_non_latin_names_are_names() {
    for (name, initial) in [
        ("研究助手", "研"),
        ("مساعد البحث", "م"),
        ("étude", "É"),
        (
            "A very long name for a bot that reads the weekly research notes and keeps a digest",
            "A",
        ),
    ] {
        let mut fx = open();
        assert!(fx.dialog.accessibility_set_name(name));
        assert_eq!(fx.dialog.name(), name);
        assert_eq!(fx.dialog.preview().1, initial, "{name}");
        assert!(fx.dialog.can_create(), "{name}");
        fx.well_formed();
        let panel = fx.dialog.with_frame(|_, panel| panel);
        assert_eq!(panel.w, DIALOG_W.value, "the name does not widen the panel");
    }
}

/// **An assistive technology presses and focuses the same controls**, and
/// what is published names every one of them.
#[test]
fn every_published_control_has_a_name_and_answers_a_click() {
    let mut fx = open();
    fx.dialog.type_name("Scout");
    let published = fx.dialog.published();
    assert_eq!(published.focus, NAME);
    assert_eq!(published.name, "Scout");
    fn walk(nodes: &[SemNode], out: &mut Vec<SemNode>) {
        for node in nodes {
            out.push(node.clone());
            walk(&node.children, out);
        }
    }
    let mut all = Vec::new();
    walk(&published.nodes, &mut all);
    assert!(
        all.iter()
            .all(|node| node.name.is_some() && node.key.is_some())
    );
    let keys: Vec<Key> = all.iter().filter_map(|node| node.key).collect();
    for key in [
        ROOT,
        TITLE,
        CLOSE,
        PREVIEW,
        NAME,
        GRID,
        OTHERS,
        RUNS,
        THIS_DEVICE,
        CLOUD,
    ] {
        assert!(keys.contains(&key), "{key:?} is published");
    }
    assert_eq!(
        keys.iter().filter(|key| face_of(**key).is_some()).count(),
        usize::from(SET),
        "twelve faces"
    );
    let face = all
        .iter()
        .find(|node| node.key == Some(face_key(2)))
        .unwrap();
    assert_eq!(face.role, Role::Button);
    assert_eq!(
        face.name,
        Some(koru_draw::el::Name::Text("Face 3".into())),
        "numbered from one"
    );
    assert_eq!(
        fx.dialog.accessibility_action(face_key(2), true),
        Outcome::Changed
    );
    assert_eq!((fx.dialog.focus(), fx.dialog.face()), (face_key(2), 2));
    assert_eq!(
        fx.dialog.accessibility_action(CREATE, false),
        Outcome::Changed
    );
    assert_eq!(fx.dialog.focus(), CREATE, "focus alone presses nothing");
    fx.nothing_sent();
}

/// Every word the dialog and the empty view say exists in all eight
/// languages, with the placeholders English has.
#[test]
fn every_new_word_is_in_all_eight_languages() {
    let keys = [
        Words::BotsNoneYet,
        Words::BotsIntroTitle,
        Words::BotsIntroBody,
        Words::BotsIntroExamples,
        Words::BotsNewBotClose,
        Words::BotsNewBotNamePlaceholder,
        Words::BotsNewBotPreview,
        Words::BotsNewBotAvatar,
        Words::BotsNewBotFace,
        Words::BotsNewBotOthers,
        Words::BotsNewBotRunsWhere,
        Words::BotsNewBotThisDevice,
        Words::BotsNewBotCloud,
        Words::BotsNewBotCloudUnavailable,
        Words::BotsNewBotLocalNote,
        Words::BotsNewBotCloudNote,
        Words::BotsNewBotCreate,
        Words::BotsChangeFolder,
        Words::AiPickerManageAgents,
        Words::SettingsTab,
        Words::BotsNewBotWorksIn,
        Words::BotsNewBotAgent,
        Words::BotsNewBotFullForm,
    ];
    assert_eq!(Lang::ALL.len(), 8);
    for key in keys {
        let slots = t(Lang::En, key).matches("{}").count();
        for lang in Lang::ALL {
            let words = t(lang, key);
            assert!(!words.trim().is_empty(), "{key:?} in {lang:?}");
            assert_eq!(words.matches("{}").count(), slots, "{key:?} in {lang:?}");
        }
    }
    // The rewordings: nothing the product does not do.
    let en = |key| t(Lang::En, key).to_lowercase();
    assert!(!en(Words::BotsNewBotLocalNote).contains("nothing leaves"));
    assert!(en(Words::BotsNewBotLocalNote).contains("model provider"));
    assert!(!en(Words::BotsNewBotCloudNote).contains("plus"));
    assert!(!en(Words::BotsIntroBody).contains("cloud"));
    for promise in ["weekly", "every ", "when "] {
        assert!(!en(Words::BotsIntroExamples).contains(promise), "{promise}");
    }
}

/// **A new dialog opens on the default face**: row 0 of the table, the gray
/// face with two dot eyes. It is the preview, the pressed tile, and what
/// Create sends when the person picks no other.
#[test]
fn a_new_dialog_opens_on_the_default_face() {
    let mut fx = open();
    assert_eq!(koru_ai_core::bots::DEFAULT_AVATAR, 0);
    assert_eq!(fx.dialog.face(), 0);
    assert_eq!(fx.dialog.preview(), (0, String::new()));
    assert_eq!(fx.selected(face_key(0)), Some(true));
    assert!(
        (1..SET).all(|face| fx.selected(face_key(face)) == Some(false)),
        "and no other"
    );
    fx.dialog.type_name("Scout");
    assert_eq!(fx.press(CREATE), Outcome::Changed);
    let BotCommand::CreateBotWithRole { config, .. } = fx.sent() else {
        panic!("a create");
    };
    assert_eq!(config.avatar, Some(0), "the default face is the face sent");
}

/// **Only the picked face moves, and only by the window's clock.** With no
/// clock handed in every face is still. With the window's clock running the
/// picked face takes that clock's pose (the preview and its tile read the
/// same one, so they move as one) and every other tile is still: the one
/// the pointer is over and the one the keyboard rests on included. Picking
/// another face moves the motion to it. *Show me others* leaves the pick
/// moving in the preview with no tile moving. A stopped clock, and reduced
/// motion, still every face.
#[test]
fn only_the_picked_face_moves_and_only_by_the_windows_clock() {
    use std::time::{Duration, Instant};

    let mut fx = open();
    let start = Instant::now();
    let later = start + Duration::from_secs(1);
    let moving = |fx: &Fx, at: Instant| -> Vec<u8> {
        (0..FACE_COUNT)
            .filter(|face| fx.dialog.pose_of(*face, at) != FacePose::STILL)
            .collect()
    };
    assert!(moving(&fx, later).is_empty(), "no clock yet: all still");

    let mut clock = FaceClock::new();
    assert!(clock.set_working(true, start));
    fx.dialog.set_face_clock(&clock);
    assert_eq!(moving(&fx, later), [0], "the picked face, and no other");
    let pose = fx.dialog.pose_of(0, later);
    assert_eq!(pose, clock.pose(0, true, later), "the window clock's pose");
    assert_eq!(pose, FacePose::at(1000, 0), "the one pose function");
    assert_eq!(
        fx.dialog.pose_of(fx.dialog.preview().0, later),
        pose,
        "the preview is that face in that pose"
    );
    // Every instant of one step draws the same pose: a repaint between two
    // steps moves nothing.
    assert_eq!(
        fx.dialog.pose_of(0, later + Duration::from_millis(20)),
        pose
    );

    // The pointer over one tile and the keyboard on another, neither
    // picked: both stay still.
    let (x, y) = fx.centre(face_key(3));
    fx.dialog.mouse_move(x, y);
    fx.dialog.set_focus(face_key(5));
    assert_eq!(fx.dialog.focus(), face_key(5));
    assert_eq!(fx.dialog.face(), 0, "neither is a pick");
    assert_eq!(moving(&fx, later), [0]);

    // Picking moves the motion.
    assert_eq!(fx.press(face_key(4)), Outcome::Changed);
    assert_eq!(moving(&fx, later), [4]);
    assert_eq!(fx.dialog.pose_of(4, later), clock.pose(4, true, later));

    // The other twelve: the pick is not among the tiles, and still moves
    // in the preview.
    assert_eq!(fx.press(OTHERS), Outcome::Changed);
    assert!((12..24).all(|face| fx.dialog.pose_of(face, later) == FacePose::STILL));
    assert_eq!(fx.dialog.preview().0, 4);
    assert_ne!(fx.dialog.pose_of(4, later), FacePose::STILL);

    // Reduced motion: every face rests.
    let mut reduced = clock.clone();
    assert!(reduced.set_reduced_motion(true));
    assert_eq!(reduced.deadline(later), None);
    fx.dialog.set_face_clock(&reduced);
    assert!(moving(&fx, later).is_empty());

    // The clock stopped (the window does that when the dialog goes).
    assert!(clock.set_working(false, later));
    assert_eq!(clock.deadline(later), None);
    fx.dialog.set_face_clock(&clock);
    assert!(moving(&fx, later).is_empty());
    fx.well_formed();
}
