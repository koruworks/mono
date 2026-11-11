// SPDX-License-Identifier: MPL-2.0

//! **The *Agent and model* card** (Bots P1.8 S7), driven through the tab
//! with a catalog link the test answers: the chip reads the draft; the
//! picker shows only the Bot's agent and folds a choice into the draft;
//! the mode list likewise; *Save* is one `Update`; and the popover's models
//! are published to assistive technology.

use super::*;
use crate::bots::catalog::{self, Catalog, CatalogLink};
use koru_ai_ui::agent_catalog::config::picker::{Readiness, Row};
use koru_ai_ui::agent_catalog::{CatalogAgent, CatalogListing, CatalogMode, CatalogModel};

fn model(id: &str, name: &str) -> CatalogModel {
    CatalogModel {
        id: id.into(),
        group: "openai".into(),
        name: Some(name.into()),
        description: None,
        group_name: None,
        legacy: false,
        levels: None,
        context: None,
        fast: Some(false),
    }
}

/// A shown card whose catalog has answered: Codex, ready, with two models
/// and two modes.
fn card() -> (Fixture, std::sync::mpsc::Receiver<catalog::Command>) {
    let mut f = Fixture::opened(unpaused());
    let (link, commands, answers) = CatalogLink::test_pair();
    f.view.catalog = Box::new(Catalog::with_link(link));
    f.view.catalog.shown();
    let provider = f.bot.config.model.provider.clone();
    f.view.catalog.want(&provider);
    f.view.shown_pass(None);
    let mut asked = Vec::new();
    while let Ok(command) = commands.try_recv() {
        asked.push(command);
    }
    for command in asked {
        match command {
            catalog::Command::Agents { seq } => answers
                .send(catalog::Answer::Agents {
                    seq,
                    result: Ok(vec![CatalogAgent {
                        id: provider.clone(),
                        key: Some("codex".into()),
                        label: "Codex".into(),
                        mark: koru_draw::brand::ProfileMark::Glyph(koru_draw::icons::ROBOT),
                    }]),
                })
                .unwrap(),
            catalog::Command::Listing { seq, agent, .. } => answers
                .send(catalog::Answer::Listing {
                    seq,
                    agent,
                    result: Ok(CatalogListing {
                        models: vec![model("fixture", "Fixture"), model("gpt-5", "GPT-5")],
                        modes: vec![
                            CatalogMode {
                                id: "ask".into(),
                                name: None,
                                description: None,
                            },
                            CatalogMode {
                                id: "auto-review".into(),
                                name: None,
                                description: None,
                            },
                        ],
                        default_mode: Some("ask".into()),
                    }),
                })
                .unwrap(),
            catalog::Command::Readiness { .. } => {}
        }
    }
    f.view.shown_pass(None);
    // The agents' answer owes each one's readiness.
    f.view.shown_pass(None);
    while let Ok(command) = commands.try_recv() {
        if let catalog::Command::Readiness { seq, agent } = command {
            answers
                .send(catalog::Answer::Readiness {
                    seq,
                    agent,
                    result: Ok(Readiness::Ready),
                })
                .unwrap();
        }
    }
    f.view.shown_pass(None);
    (f, commands)
}

#[test]
fn the_card_chooses_a_model_and_a_mode_for_new_conversations_and_saves_once() {
    let (mut f, _catalog) = card();
    let lang = f.view.lang;
    assert_eq!(f.view.model_chip_words(), "Codex, Fixture");
    assert_eq!(
        f.view.mode_chip_words().as_deref(),
        Some(t(lang, Words::AiAgentModeAskApproval)),
        "the agent's own start, in Koru's words"
    );
    assert!(f.view.agent_readiness_words().is_none(), "ready");

    // The picker: the Bot's agent alone, no rail, every model choosable.
    assert!(f.view.perform(Act::OpenModelPicker));
    let (hidden, rows) = f
        .view
        .agent_picker_read(|picker| (picker.rail_hidden(), picker.rows()))
        .unwrap();
    assert!(hidden, "the agent is fixed after creation (O42)");
    let gpt = rows
        .iter()
        .position(|row| *row == Row::Model("gpt-5".into()))
        .expect("GPT-5 is listed");
    f.view.agent_picker_run(|view| view.choose_row(gpt));
    let draft = f.view.settings().model_shown().unwrap();
    assert_eq!(draft.model.requested_model, "gpt-5");
    assert_eq!(draft.model.provider, f.bot.config.model.provider);
    assert_eq!(f.view.model_chip_words(), "Codex, GPT-5");

    // The popover is published: a list of the agent's models, the chosen
    // one selected.
    let mut all = Vec::new();
    fn walk(controls: &[Control], out: &mut Vec<Control>) {
        for control in controls {
            out.push(control.clone());
            walk(&control.children, out);
        }
    }
    walk(&f.view.controls(), &mut all);
    let list = all
        .iter()
        .find(|control| control.role == ControlRole::List && control.label == "Codex")
        .expect("the picker's list");
    let chosen = list
        .children
        .iter()
        .find(|item| item.label == "GPT-5")
        .expect("GPT-5's row");
    assert_eq!(chosen.role, ControlRole::ListItem);
    assert_eq!(chosen.selected, Some(true));
    assert!(
        all.iter()
            .any(|control| control.role == ControlRole::TextInput && control.value.is_some()),
        "the search field, with its text"
    );

    // Esc closes it, and the keyboard is back on the chip.
    key(&mut f.view, named(winit::keyboard::NamedKey::Escape), None);
    assert!(!f.view.agent_popover_open());
    assert_eq!(f.view.focus, Some(MODEL_CHIP));

    // The mode list: Down, Enter chooses *Approve for me*.
    assert!(f.view.perform(Act::OpenModes));
    key(
        &mut f.view,
        named(winit::keyboard::NamedKey::ArrowDown),
        None,
    );
    key(&mut f.view, named(winit::keyboard::NamedKey::Enter), None);
    assert!(!f.view.agent_popover_open());
    assert_eq!(
        f.view.settings().model_shown().unwrap().mode.as_deref(),
        Some("auto-review")
    );

    // *Save*: one Update of what was read, with the model and the mode.
    assert!(f.view.perform(Act::SaveModel));
    let (_, command) = f.wrote();
    let BotCommand::Update {
        expected_revision,
        config,
        ..
    } = command
    else {
        panic!("an update");
    };
    assert_eq!(expected_revision, f.bot.revision);
    assert_eq!(config.model.requested_model, "gpt-5");
    assert_eq!(config.mode.as_deref(), Some("auto-review"));
    assert_eq!(config.name, f.bot.config.name);
    // While it is on its way the chips are inert and *Saving…* shows.
    assert!(!f.view.perform(Act::SaveModel));
    let note = all
        .iter()
        .find(|control| control.key == MODEL_NOTE)
        .expect("the note");
    assert_eq!(note.label, t(lang, Words::BotsAppliesToNewConversations));
}

/// **An agent that is not ready says so by its name**, and nothing is
/// chosen in its place.
#[test]
fn an_agent_that_is_not_ready_is_named_and_kept() {
    let (mut f, _catalog) = card();
    f.view
        .catalog
        .force_readiness_for_test(&f.bot.config.model.provider, Readiness::SignedOut);
    let words = f.view.agent_readiness_words().expect("said");
    assert!(words.contains("Codex"), "{words}");
    assert_eq!(
        f.view.settings().model_shown().unwrap().model.provider,
        f.bot.config.model.provider
    );
}
