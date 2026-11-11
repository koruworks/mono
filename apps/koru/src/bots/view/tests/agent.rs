// SPDX-License-Identifier: MPL-2.0

//! **The *Agent and model* card** (Bots P1.8 S7), driven through the tab
//! with a catalog link the test answers: the chip reads the draft; the
//! picker shows only the Bot's agent and folds a choice into the draft;
//! the mode list likewise; *Save* is one `Update`; and the popover's models
//! are published to assistive technology.

use super::*;
use crate::bots::catalog::{self, Catalog, CatalogLink};
use koru_ai_ui::agent_catalog::config::modes;
use koru_ai_ui::agent_catalog::config::picker::{Readiness, Row};
use koru_ai_ui::agent_catalog::{CatalogAgent, CatalogListing, CatalogMode, CatalogModel};
use koru_ui::split_button::SplitTone;

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
    card_with(unpaused(), &["ask", "auto-review"])
}

/// The same over `bot`, whose agent lists `modes`, in that order, starting
/// in *ask*.
fn card_with(bot: Bot, modes: &[&str]) -> (Fixture, std::sync::mpsc::Receiver<catalog::Command>) {
    let mut f = Fixture::opened(bot);
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
                        modes: modes
                            .iter()
                            .map(|id| CatalogMode {
                                id: (*id).into(),
                                name: None,
                                description: None,
                            })
                            .collect(),
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
        f.view.mode_chip_row().map(|row| row.title).as_deref(),
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

/// One mode as the chip must wear it: the mode the Bot's read names, the
/// mode the chip then shows, its color as `tone_of` decides it, the inks
/// the chip is drawn in (icon, label) and its icon.
struct Worn {
    held: &'static str,
    shown: &'static str,
    tone: SplitTone,
    icon_ink: &'static str,
    label_ink: &'static str,
    icon: char,
}

/// Codex's asking mode, its accept-edits equivalent (automatic review), a
/// plain Auto it does not name, Full access, a mode Koru does not know, and
/// a mode it no longer lists (the chip falls back to the first).
const WORN: &[Worn] = &[
    Worn {
        held: "ask",
        shown: "ask",
        tone: SplitTone::Accent,
        icon_ink: "Ink::Accent",
        label_ink: "Ink::Primary",
        icon: koru_draw::icons::QUESTION,
    },
    Worn {
        held: "auto-review",
        shown: "auto-review",
        tone: SplitTone::Success,
        icon_ink: "Ink::Success",
        label_ink: "Ink::Primary",
        icon: koru_draw::icons::ROBOT,
    },
    Worn {
        held: "auto",
        shown: "auto",
        tone: SplitTone::Neutral,
        icon_ink: "Ink::Primary",
        label_ink: "Ink::Primary",
        icon: koru_draw::icons::FADERS,
    },
    Worn {
        held: "full-access",
        shown: "full-access",
        tone: SplitTone::Warning,
        icon_ink: "Ink::Notice",
        label_ink: "Ink::Notice",
        icon: koru_draw::icons::WARNING,
    },
    Worn {
        held: "custom",
        shown: "custom",
        tone: SplitTone::Accent,
        icon_ink: "Ink::Accent",
        label_ink: "Ink::Primary",
        icon: koru_draw::icons::FADERS,
    },
    Worn {
        held: "gone",
        shown: "ask",
        tone: SplitTone::Accent,
        icon_ink: "Ink::Accent",
        label_ink: "Ink::Primary",
        icon: koru_draw::icons::QUESTION,
    },
];

/// Every mode the fixture agent lists, in its order.
const LISTED: &[&str] = &["ask", "auto-review", "auto", "full-access", "custom"];

/// The chip's icon as the inspector records it.
fn glyph(icon: char) -> String {
    format!("U+{:04X}", icon as u32)
}

/// **The mode chip wears the mode's color** (P1.8 §20, the owner's
/// decision): its icon in the color `tone_of` decides for the agent and the
/// mode — the one a chat's send button and the list's row wear — its label
/// in the reading ink but for *Full access*, which warns in both; its name
/// says the mode in words and nothing about the color.
#[test]
fn the_settings_mode_chip_wears_the_modes_color() {
    for worn in WORN {
        let mut bot = unpaused();
        bot.config.mode = Some(worn.held.into());
        let (f, _catalog) = card_with(bot, LISTED);
        let agent = f.bot.config.model.provider.as_str().to_owned();
        assert_eq!(
            modes::tone_of(&agent, worn.shown),
            worn.tone,
            "{}: the one decision",
            worn.held
        );
        let drawn = mode_chip_drawn(&f.view);
        assert_eq!(ink_of(&drawn, "lead ink"), worn.icon_ink, "{}", worn.held);
        assert_eq!(ink_of(&drawn, "label ink"), worn.label_ink, "{}", worn.held);
        assert_eq!(
            drawn.get("lead icon"),
            Some(&glyph(worn.icon)),
            "{}",
            worn.held
        );
        let title = modes::rows(f.view.lang, &agent, &offers(LISTED))
            .into_iter()
            .find(|row| row.id == worn.shown)
            .expect("listed")
            .title;
        assert_eq!(
            published(&f.view, MODE_CHIP).as_deref(),
            Some(title.as_str()),
            "{}: named in words, the color not in the name",
            worn.held
        );
    }
}

/// The fixture's modes as the control takes them.
fn offers<'a>(ids: &'a [&'a str]) -> Vec<modes::ModeOffer<'a>> {
    ids.iter()
        .map(|id| modes::ModeOffer {
            id,
            name: None,
            description: None,
        })
        .collect()
}

/// **The chip's states keep working in color**: focused it is ringed and
/// still wears the mode's color; with its agent not ready it is still
/// there in color; while a save is on its way it is inert and reads as
/// inert — the icon and the words in the inert ink, as any inert action.
#[test]
fn the_toned_mode_chip_keeps_its_states_and_inert_reads_as_inert() {
    let mut bot = unpaused();
    bot.config.mode = Some("full-access".into());
    let (mut f, _catalog) = card_with(bot, LISTED);

    f.view.focus = Some(MODE_CHIP);
    let drawn = mode_chip_drawn(&f.view);
    assert_eq!(
        drawn.get("hovered / focused").map(String::as_str),
        Some("false / true")
    );
    assert_eq!(ink_of(&drawn, "lead ink"), "Ink::Notice");

    f.view
        .catalog
        .force_readiness_for_test(&f.bot.config.model.provider, Readiness::SignedOut);
    assert!(f.view.agent_readiness_words().is_some(), "not ready");
    assert_eq!(ink_of(&mode_chip_drawn(&f.view), "lead ink"), "Ink::Notice");
    f.view
        .catalog
        .force_readiness_for_test(&f.bot.config.model.provider, Readiness::Ready);

    // A change, saved: the chips are inert while it is on its way.
    assert!(f.view.perform(Act::OpenModes));
    key(&mut f.view, named(winit::keyboard::NamedKey::ArrowUp), None);
    key(&mut f.view, named(winit::keyboard::NamedKey::Enter), None);
    assert!(f.view.perform(Act::SaveModel));
    let _ = f.wrote();
    let drawn = mode_chip_drawn(&f.view);
    assert_eq!(
        drawn.get("implementation").map(String::as_str),
        Some("koru_ui::button::ActionButton::draw_disabled"),
        "inert"
    );
    assert_eq!(
        ink_of(&drawn, "lead ink"),
        "Ink::Secondary",
        "no color while inert"
    );
    assert_eq!(ink_of(&drawn, "label ink"), "Ink::Secondary");
}
