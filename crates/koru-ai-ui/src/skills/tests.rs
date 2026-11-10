// SPDX-License-Identifier: MPL-2.0

//! The composer's `$` skill list (D121), driven as a person drives it: a
//! `$` typed, the agent's answer delivered as the reader would deliver it,
//! keys and presses. No agent, session host or service is anywhere near.

use std::sync::mpsc::Receiver;
use std::time::Duration;

use koru_ai_core::{AiProviderId, Chat, ModelSelection, ReasoningChoice, Timestamp};
use koru_ai_wire::{AgentCommandKindView, AgentCommandView, ChatTranscript};
use koru_i18n::Lang;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use super::*;
use crate::composer::{dollar_before, slash_before};
use crate::reader::{Command, Reader, Update};

fn transcript(provider: &str, folder: Option<&str>) -> ChatTranscript {
    let chat_id = koru_ai_core::ChatId::new();
    let now = Timestamp::from_millis(1);
    ChatTranscript {
        chat: Chat {
            id: chat_id,
            title: "Agent chat".into(),
            private: false,
            current_model: ModelSelection {
                provider: AiProviderId::parse(provider).unwrap(),
                requested_model: koru_ai_core::AGENT_DEFAULT_MODEL.into(),
                default_reasoning: ReasoningChoice::Default,
            },
            branched_from: None,
            agent_working_folder: folder.map(str::to_owned),
            agent_mode: None,
            agent_fast: None,
            pinned_at: None,
            profile_id: None,
            created_at: now,
            updated_at: now,
        },
        tool_policy: koru_ai_core::ChatToolPolicy {
            chat_id,
            mode: koru_ai_core::ToolMode::Manual,
            workspace_ids: Vec::new(),
        },
        tool_activities: Vec::new(),
        notices: Vec::new(),
        messages: Vec::new(),
        blocks: Vec::new(),
        pinned_documents: Vec::new(),
        feedback: Vec::new(),
        usage: Vec::new(),
        sources: Vec::new(),
        reasoning_control: None,
        agent_commands: None,
        attachments: Vec::new(),
    }
}

fn skill(
    name: &str,
    display: Option<&str>,
    line: &str,
    kind: AgentCommandKindView,
) -> AgentCommandView {
    AgentCommandView {
        name: name.into(),
        description: line.into(),
        display_name: display.map(str::to_owned),
        kind,
        input_hint: None,
    }
}

/// The agent's list in its order: the folder's, the person's, its own.
fn listing() -> Vec<AgentCommandView> {
    vec![
        skill(
            "review-pr",
            None,
            "Review the open pull request",
            AgentCommandKindView::RepoSkill,
        ),
        skill(
            "release-notes",
            Some("Release Notes"),
            "Write the notes for a release",
            AgentCommandKindView::UserSkill,
        ),
        skill(
            "pr-review-legacy",
            None,
            "",
            AgentCommandKindView::UserSkill,
        ),
        skill(
            "imagegen",
            Some("Image Gen"),
            "Generate an image",
            AgentCommandKindView::SystemSkill,
        ),
    ]
}

/// A Codex chat in `D:/work`, its composer laid out, with a reader whose
/// commands the test reads.
fn surface() -> (ChatSurface, Receiver<Command>) {
    let mut surface = ChatSurface::new();
    surface.lang = Lang::En;
    let (reader, commands, _updates) = Reader::test_pair();
    surface.reader = Some(reader);
    surface.transcript = Some(transcript("agent-codex", Some("D:/work")));
    surface.set_viewport(Rect::new(0.0, 0.0, 900.0, 700.0));
    surface.composer_rect();
    (surface, commands)
}

fn type_text(surface: &mut ChatSurface, text: &str) {
    surface.composer.set_text(text);
    surface.sync_skill_list();
}

fn answer(surface: &mut ChatSurface, skills: Vec<AgentCommandView>) {
    surface.apply_update(Update::AgentCommands {
        provider: AiProviderId::parse("agent-codex").unwrap(),
        folder: Some("D:/work".into()),
        commands: Some(skills),
    });
}

fn press(surface: &mut ChatSurface, named: NamedKey) -> bool {
    surface.key(&Key::Named(named), None, ModifiersState::empty())
}

fn asks(commands: &Receiver<Command>) -> Vec<(String, Option<String>)> {
    let mut asks = Vec::new();
    while let Ok(command) = commands.recv_timeout(Duration::from_millis(20)) {
        if let Command::DiscoverCommands(provider, folder) = command {
            asks.push((provider.as_str().to_owned(), folder));
        }
    }
    asks
}

fn shown(surface: &ChatSurface) -> Vec<String> {
    surface
        .skill_specs()
        .into_iter()
        .map(|spec| spec.title)
        .collect()
}

/// **A `$` token** is a `$` at the start of a line or after a space, then
/// name characters up to the caret.
#[test]
fn a_dollar_token_starts_a_line_or_follows_a_space() {
    assert_eq!(dollar_before("$", 1), Some((0, "")));
    assert_eq!(dollar_before("Use $re", 7), Some((4, "re")));
    assert_eq!(dollar_before("Use $pdf:pdf", 12), Some((4, "pdf:pdf")));
    assert_eq!(dollar_before("Use $re", 5), Some((4, "")));
    assert_eq!(dollar_before("a$b", 3), None, "inside a word");
    assert_eq!(dollar_before("5$", 2), None, "a price");
    assert_eq!(dollar_before("$$", 2), None);
    assert_eq!(dollar_before("$re now", 7), None, "the caret left it");
    assert_eq!(dollar_before("plain", 5), None);
}

#[test]
fn slash_token_starts_a_line_or_follows_a_space() {
    assert_eq!(slash_before("/", 1), Some((0, "")));
    assert_eq!(slash_before("Use /review-pr", 14), Some((4, "review-pr")));
    assert_eq!(slash_before("a/b", 3), None);
    assert_eq!(slash_before("https://example", 15), None);
    assert_eq!(slash_before("/review now", 11), None);
}

#[test]
fn slash_lists_commands_and_skills_and_keeps_selection_at_its_position() {
    let (mut surface, commands) = surface();
    type_text(&mut surface, "Use /rev");
    answer(
        &mut surface,
        vec![
            skill("review-pr", None, "Review", AgentCommandKindView::Command),
            skill("revise", None, "Revise", AgentCommandKindView::RepoSkill),
        ],
    );
    assert_eq!(shown(&surface), ["review-pr", "revise"]);
    assert!(press(&mut surface, NamedKey::Enter));
    let message = surface.composer.message();
    assert_eq!(message.markdown, "Use [[koru://command/review-pr]] ");
    assert!(message.skills.is_empty());

    type_text(&mut surface, "Use $rev");
    assert_eq!(shown(&surface), ["revise"], "dollar excludes commands");
    assert!(press(&mut surface, NamedKey::Enter));
    assert_eq!(surface.composer.message().skills, ["revise"]);
    let _ = asks(&commands);
}

#[test]
fn plus_menu_command_and_skill_doors_open_the_same_picker() {
    use crate::composer_plus::PlusItem;

    let (mut surface, _commands) = surface();
    surface.open_plus_menu();
    let command_index = surface
        .plus_menu
        .as_ref()
        .unwrap()
        .items()
        .iter()
        .position(|item| matches!(item, PlusItem::Commands(_)))
        .unwrap();
    assert!(surface.choose_plus_item(command_index));
    assert_eq!(surface.composer.text(), "/");
    answer(
        &mut surface,
        vec![skill(
            "review-pr",
            None,
            "Review",
            AgentCommandKindView::Command,
        )],
    );
    assert_eq!(shown(&surface), ["review-pr"]);

    surface.composer.set_text("Then ");
    surface.open_plus_menu();
    let skill_index = surface
        .plus_menu
        .as_ref()
        .unwrap()
        .items()
        .iter()
        .position(|item| matches!(item, PlusItem::Skills(_)))
        .unwrap();
    assert!(surface.choose_plus_item(skill_index));
    assert_eq!(surface.composer.text(), "Then $");
}

#[test]
fn transcript_live_listing_overrides_bootstrap_and_empty_is_authoritative() {
    let (mut surface, commands) = surface();
    answer(
        &mut surface,
        vec![skill(
            "old-skill",
            None,
            "",
            AgentCommandKindView::RepoSkill,
        )],
    );
    surface.transcript.as_mut().unwrap().agent_commands = Some(vec![skill(
        "current-command",
        None,
        "",
        AgentCommandKindView::Command,
    )]);
    type_text(&mut surface, "/");
    assert_eq!(shown(&surface), ["current-command"]);
    assert!(asks(&commands).is_empty(), "a live list needs no probe");

    surface.transcript.as_mut().unwrap().agent_commands = Some(Vec::new());
    surface.sync_skill_list();
    assert!(
        surface.skill_list_open(),
        "empty live list has an empty state"
    );
    assert_eq!(surface.skill_content_state(), elements::ContentState::Empty);
    surface.transcript.as_mut().unwrap().agent_commands = None;
    surface.sync_skill_list();
    assert_eq!(shown(&surface), ["old-skill"]);
}

#[test]
fn second_surface_loads_live_listing_and_stream_updates_it() {
    let (mut first, _commands) = surface();
    first.transcript.as_mut().unwrap().agent_commands = Some(vec![skill(
        "first",
        None,
        "",
        AgentCommandKindView::Command,
    )]);
    let wire = serde_json::to_value(first.transcript.as_ref().unwrap()).unwrap();
    let (mut second, _commands) = surface();
    second.transcript = Some(serde_json::from_value(wire).unwrap());
    type_text(&mut second, "/");
    assert_eq!(shown(&second), ["first"]);

    let chat = second.transcript.as_ref().unwrap().chat.id;
    second.apply_update(Update::Event(koru_ai_wire::StreamEvent {
        version: koru_ai_wire::AI_STREAM_VERSION,
        sequence: 1,
        event: koru_ai_wire::PublicAiEvent::AgentCommands {
            attempt_id: koru_ai_core::InferenceAttemptId::new(),
            chat,
            commands: vec![skill("second", None, "", AgentCommandKindView::Command)],
        },
    }));
    assert_eq!(shown(&second), ["second"]);
}

#[test]
fn two_selected_commands_keep_their_mid_message_order() {
    let (mut surface, _commands) = surface();
    type_text(&mut surface, "First /one");
    answer(
        &mut surface,
        vec![
            skill("one", None, "", AgentCommandKindView::Command),
            skill("two", None, "", AgentCommandKindView::Command),
        ],
    );
    assert!(press(&mut surface, NamedKey::Enter));
    for character in "then /two".chars() {
        let typed = character.to_string();
        surface.key(
            &Key::Character(typed.as_str().into()),
            Some(&typed),
            ModifiersState::empty(),
        );
    }
    assert_eq!(shown(&surface), ["two"]);
    assert!(press(&mut surface, NamedKey::Enter));
    assert_eq!(
        surface.composer.message().markdown,
        "First [[koru://command/one]] then [[koru://command/two]] "
    );
}

/// **The filter**: name or display name, ignoring case; those that start
/// with the query first, then those that contain it, each in the agent's
/// order.
#[test]
fn matches_put_prefixes_first_then_the_rest_in_the_agents_order() {
    let skills = listing();
    assert_eq!(matches(&skills, ""), vec![0, 1, 2, 3]);
    assert_eq!(
        matches(&skills, "re"),
        vec![0, 1, 2],
        "release-notes by name"
    );
    assert_eq!(matches(&skills, "pr"), vec![2, 0], "prefix, then within");
    assert_eq!(
        matches(&skills, "IMAGE"),
        vec![3],
        "the display name, any case"
    );
    assert!(matches(&skills, "zzz").is_empty());
    assert_eq!(title(&skills[1]), "Release Notes");
    assert_eq!(title(&skills[0]), "review-pr");
}

/// **`$` opens immediately**: the chat's agent and folder are asked once;
/// placeholders give way to filtered rows, and the list shuts when the caret
/// leaves the token.
#[test]
fn a_dollar_opens_while_the_list_is_loading() {
    let (mut surface, commands) = surface();
    type_text(&mut surface, "Use $");
    assert!(surface.skill_list_open());
    assert_eq!(
        surface.skill_content_state(),
        elements::ContentState::Loading
    );
    let (panel, specs, frame) = surface.skill_frame().unwrap();
    assert!(specs.is_empty());
    assert_eq!(frame.named(elements::LOADING_ROW).count(), 3);
    assert!(
        frame
            .named(elements::LOADING_ROW)
            .all(|row| panel.contains(row.rect.x, row.rect.y))
    );
    assert_eq!(surface.skill_row_at(panel.x + 30.0, panel.y + 30.0), None);
    assert!(
        press(&mut surface, NamedKey::Enter),
        "loading does not send"
    );
    assert_eq!(surface.composer.text(), "Use $");
    assert_eq!(
        asks(&commands),
        vec![("agent-codex".to_owned(), Some("D:/work".to_owned()))]
    );
    answer(&mut surface, listing());
    assert!(surface.skill_list_open());
    assert_eq!(
        shown(&surface),
        [
            "review-pr",
            "Release Notes",
            "pr-review-legacy",
            "Image Gen"
        ]
    );
    assert!(surface.skill_panel().is_some());

    type_text(&mut surface, "Use $pr");
    assert_eq!(shown(&surface), ["pr-review-legacy", "review-pr"]);
    assert!(asks(&commands).is_empty(), "the same $ asks once");

    type_text(&mut surface, "Use $zzz");
    assert!(surface.skill_list_open(), "an unmatched query has feedback");
    assert_eq!(surface.skill_content_state(), elements::ContentState::Empty);
    type_text(&mut surface, "Use $pr and");
    assert!(!surface.skill_list_open(), "the caret left the token");
    assert!(surface.skill_panel().is_none());
}

#[test]
fn opening_plus_closes_the_command_panel_under_it() {
    let (mut surface, _commands) = surface();
    type_text(&mut surface, "Use $");
    assert!(surface.skill_list_open());
    let plus = surface.plus_button();
    assert_eq!(
        surface.plus_menu_mouse_down(plus.x + plus.w * 0.5, plus.y + plus.h * 0.5),
        Some(true)
    );
    assert!(surface.plus_menu.is_some());
    assert!(!surface.skill_list_open());
    surface.sync_skill_list();
    assert!(!surface.skill_list_open(), "the same token stays dismissed");
}

/// **Enter picks while the list is open, and sends once it is shut**: the
/// pick is a chip that is sent as `$name`, with the skill's name beside the
/// text, and Backspace takes the chip back whole.
#[test]
fn enter_picks_a_chip_that_is_sent_as_its_name() {
    let (mut surface, commands) = surface();
    type_text(&mut surface, "Use $re");
    answer(&mut surface, listing());
    assert!(press(&mut surface, NamedKey::ArrowDown));
    assert!(press(&mut surface, NamedKey::Enter), "picks, never sends");
    assert!(!surface.skill_list_open());
    let message = surface.composer.message();
    assert_eq!(message.markdown, "Use [[koru://skill/release-notes]] ");
    assert_eq!(message.skills, vec!["release-notes".to_owned()]);

    // Backspace: the space, then the chip whole.
    assert!(press(&mut surface, NamedKey::Backspace));
    assert!(press(&mut surface, NamedKey::Backspace));
    let message = surface.composer.message();
    assert_eq!(message.markdown, "Use ");
    assert!(message.skills.is_empty());

    // Tab picks too; then Enter, with the list shut, sends.
    type_text(&mut surface, "Use $rev");
    assert!(surface.skill_list_open());
    assert!(press(&mut surface, NamedKey::Tab));
    assert_eq!(
        surface.composer.message().skills,
        vec!["review-pr".to_owned()]
    );
    let _ = asks(&commands);
    assert!(press(&mut surface, NamedKey::Enter));
    let request = loop {
        match commands.recv_timeout(Duration::from_millis(200)).unwrap() {
            Command::Send(request) => break request,
            _ => continue,
        }
    };
    assert_eq!(request.markdown, "Use [[koru://skill/review-pr]]");
    assert_eq!(request.skills, vec!["review-pr".to_owned()]);
}

/// **Escape shuts the list for that `$`**: typing on in the same token
/// leaves it shut; a new `$` opens it again.
#[test]
fn escape_shuts_the_list_until_a_new_dollar() {
    let (mut surface, _commands) = surface();
    type_text(&mut surface, "$");
    answer(&mut surface, listing());
    assert!(surface.skill_list_open());
    assert!(press(&mut surface, NamedKey::Escape));
    assert!(!surface.skill_list_open());
    // Typed on, key by key: the same row, the same `$`.
    let typed = |surface: &mut ChatSurface, text: &str| {
        for character in text.chars() {
            let character = character.to_string();
            surface.key(
                &Key::Character(character.as_str().into()),
                Some(&character),
                ModifiersState::empty(),
            );
        }
    };
    typed(&mut surface, "r");
    assert_eq!(surface.composer.text(), "$r");
    assert!(!surface.skill_list_open(), "the same $");
    typed(&mut surface, " $");
    assert!(surface.skill_list_open(), "a new $");
}

/// **Up and Down wrap, and the rows shown follow the keyboard**: past the
/// rows that fit, the list scrolls to the keyboard's row.
#[test]
fn the_keyboard_moves_through_more_skills_than_fit() {
    let (mut surface, _commands) = surface();
    let many: Vec<AgentCommandView> = (0..12)
        .map(|index| {
            skill(
                &format!("skill-{index:02}"),
                None,
                "",
                AgentCommandKindView::UserSkill,
            )
        })
        .collect();
    type_text(&mut surface, "$");
    answer(&mut surface, many);
    assert_eq!(surface.skill_specs().len(), VISIBLE_ROWS);
    assert!(press(&mut surface, NamedKey::ArrowUp), "wraps to the last");
    let specs = surface.skill_specs();
    assert_eq!(specs.last().unwrap().title, "skill-11");
    assert!(specs.last().unwrap().lit);
    assert!(
        press(&mut surface, NamedKey::ArrowDown),
        "wraps to the first"
    );
    assert_eq!(surface.skill_specs()[0].title, "skill-00");
    assert!(surface.skill_specs()[0].lit);
}

/// **An empty list has feedback**. A chat with no local agent leaves `$` as a
/// character and never asks.
#[test]
fn without_skills_the_panel_reports_empty() {
    let (mut surface, commands) = surface();
    type_text(&mut surface, "$");
    answer(&mut surface, Vec::new());
    assert!(surface.skill_list_open());
    assert_eq!(surface.skill_content_state(), elements::ContentState::Empty);
    assert!(press(&mut surface, NamedKey::Escape) || !surface.skill_list_open());

    let (mut plain, commands_plain) = {
        let mut surface = ChatSurface::new();
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        surface.transcript = Some(transcript("openai", None));
        (surface, commands)
    };
    type_text(&mut plain, "$");
    assert!(!plain.skill_list_open());
    assert!(asks(&commands_plain).is_empty(), "no local agent, no ask");
    drop(commands);
}

#[test]
fn a_slash_opens_while_commands_are_loading_and_escape_closes_it() {
    let (mut surface, commands) = surface();
    type_text(&mut surface, "/");
    assert!(surface.skill_list_open());
    assert_eq!(
        surface.skill_content_state(),
        elements::ContentState::Loading
    );
    assert!(surface.skill_panel().is_some());
    assert!(press(&mut surface, NamedKey::Escape));
    assert!(!surface.skill_list_open());
    assert_eq!(asks(&commands).len(), 1);
}

#[test]
fn priming_warms_the_list_without_a_trigger() {
    let (mut surface, commands) = surface();
    surface.prime_skill_list();
    assert_eq!(asks(&commands).len(), 1);
    answer(&mut surface, listing());
    type_text(&mut surface, "$");
    assert_eq!(surface.skill_content_state(), elements::ContentState::Rows);
    assert_eq!(shown(&surface).len(), 4);
    assert!(asks(&commands).is_empty(), "fresh cached list is reused");
}

#[test]
fn switching_agent_sources_drops_the_old_list_and_primes_the_new_one() {
    let (mut surface, commands) = surface();
    type_text(&mut surface, "$");
    answer(&mut surface, listing());
    let mut chat = surface.transcript.as_ref().unwrap().chat.clone();
    chat.current_model.provider = AiProviderId::parse("agent-claude").unwrap();
    surface.apply_update(Update::ModelUpdated(chat, None));
    assert!(surface.skill_list_open());
    assert!(shown(&surface).is_empty());
    assert_eq!(
        surface.skill_content_state(),
        elements::ContentState::Loading
    );
    answer(&mut surface, listing());
    assert_eq!(
        surface.skill_content_state(),
        elements::ContentState::Loading,
        "late Codex answer cannot replace Claude's list"
    );
    surface.apply_update(Update::AgentCommands {
        provider: AiProviderId::parse("agent-claude").unwrap(),
        folder: Some("D:/work".into()),
        commands: Some(vec![skill(
            "claude-only",
            None,
            "",
            AgentCommandKindView::UserSkill,
        )]),
    });
    assert_eq!(shown(&surface), ["claude-only"]);
    assert_eq!(
        asks(&commands),
        vec![
            ("agent-codex".to_owned(), Some("D:/work".to_owned())),
            ("agent-claude".to_owned(), Some("D:/work".to_owned())),
        ]
    );
}

/// **A press on a row picks it**; a press elsewhere shuts the list and goes
/// on.
#[test]
fn a_press_on_a_row_picks_it() {
    let (mut surface, _commands) = surface();
    type_text(&mut surface, "Ask $");
    answer(&mut surface, listing());
    let panel = surface.skill_panel().unwrap();
    let row_h = elements::ROW_HEIGHT.value;
    let inset = elements::PANEL_INSET.token.px(Profile::Desktop);
    // The second row: Release Notes.
    let (x, y) = (panel.x + panel.w * 0.5, panel.y + inset + row_h * 1.5);
    assert_eq!(surface.skill_row_at(x, y), Some(1));
    assert_eq!(surface.skill_hover_at(x, y), Some((20, 1)));
    // The arrow over a row, never a hand or the transcript's I-beam
    // (ADR-215).
    surface.pointer = (x, y);
    assert_eq!(surface.cursor_icon(), winit::window::CursorIcon::Default);
    assert!(surface.mouse_down(x, y));
    assert_eq!(
        surface.composer.message().skills,
        vec!["release-notes".to_owned()]
    );

    type_text(&mut surface, "Ask $");
    assert!(surface.skill_list_open());
    assert_eq!(
        surface.skill_mouse_down(1.0, 1.0),
        None,
        "elsewhere goes on"
    );
    assert!(!surface.skill_list_open());
}

/// A description far too long for a row's one line.
const LONG: &str = "Review the open pull request against the repository's \
    contribution rules, then write up every finding with the file and line \
    it is about, and what to change before it can land.";

/// The list with a long description first, a short one second, none third.
fn described() -> Vec<AgentCommandView> {
    vec![
        skill("review-pr", None, LONG, AgentCommandKindView::RepoSkill),
        skill(
            "imagegen",
            Some("Image Gen"),
            "Generate an image",
            AgentCommandKindView::SystemSkill,
        ),
        skill("bare", None, "", AgentCommandKindView::UserSkill),
    ]
}

/// The middle of the `index`th row shown.
fn row_point(surface: &ChatSurface, index: usize) -> (f32, f32) {
    let panel = surface.skill_panel().unwrap();
    let inset = elements::PANEL_INSET.token.px(Profile::Desktop);
    (
        panel.x + panel.w * 0.5,
        panel.y + inset + elements::ROW_HEIGHT.value * (index as f32 + 0.5),
    )
}

/// Past the hover delay and the fade in, as the window's clock would step it.
fn settle(surface: &mut ChatSurface) -> Instant {
    let motion = koru_draw::theme::motion::tooltip();
    let later =
        Instant::now() + motion.hover_delay + motion.enter_duration + Duration::from_millis(10);
    surface.maintain_skill_tooltip(later);
    surface.maintain_skill_tooltip(later);
    later
}

/// **A long description, after the app's hover delay, in full beside the
/// list** (the user: *on long hover, show the full description as
/// tooltip*): the shared tooltip, wrapped, standing clear of the list
/// rather than over the rows. Nothing before the delay.
#[test]
fn a_rested_pointer_shows_an_elided_description_whole_beside_the_list() {
    let (mut surface, _commands) = surface();
    type_text(&mut surface, "Use $");
    answer(&mut surface, described());
    let panel = surface.skill_panel().unwrap();
    let (x, y) = row_point(&surface, 0);

    let tip = surface.skill_tooltip_at(x, y).expect("elided: a tooltip");
    assert_eq!(tip.label, LONG, "the whole description");
    assert!(tip.wrap, "wrapped, not elided again");
    assert_eq!(tip.placement, koru_ui::tooltip::Placement::Beside);
    assert_eq!((tip.anchor.x, tip.anchor.w), (panel.x, panel.w));

    let before = Instant::now();
    surface.mouse_move(x, y);
    let delay = koru_draw::theme::motion::tooltip().hover_delay;
    let due = surface
        .skill_tooltip()
        .maintenance_deadline()
        .expect("waiting");
    assert!(due >= before + delay, "the app's hover delay, not sooner");
    assert_eq!(surface.skill_tooltip().visibility(before), 0.0);

    let later = settle(&mut surface);
    assert_eq!(surface.skill_tooltip().visibility(later), 1.0);
    let tooltip = surface.skill_tooltip().clone();
    let drawn = tooltip
        .draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut surface.engine,
            surface.viewport,
            later,
        )
        .expect("drawn");
    assert!(
        drawn.x >= panel.x + panel.w || drawn.x + drawn.w <= panel.x,
        "beside the list, never over it: {drawn:?} by {panel:?}"
    );
    assert!(drawn.w <= koru_ui::tooltip::MAX_W + 0.5);
    assert!(drawn.h > 26.0, "wrapped onto more than one line");
}

/// **Only a description the row cut short**: one that fits its line, and a
/// row with none, raise nothing.
#[test]
fn a_description_that_fits_raises_nothing() {
    let (mut surface, _commands) = surface();
    type_text(&mut surface, "Use $");
    answer(&mut surface, described());
    let (x, y) = row_point(&surface, 1);
    assert!(surface.skill_tooltip_at(x, y).is_none(), "fits its line");
    surface.mouse_move(x, y);
    assert!(!surface.skill_tooltip().is_present());
    let (x, y) = row_point(&surface, 2);
    assert!(surface.skill_tooltip_at(x, y).is_none(), "no description");
}

/// **It goes when the pointer leaves the row, a key is pressed, the list
/// scrolls or closes** — and the keyboard's row never raises one.
#[test]
fn leaving_a_key_a_scroll_or_closing_takes_the_description_away() {
    let (mut surface, _commands) = surface();
    type_text(&mut surface, "Use $");
    answer(&mut surface, described());
    let (x, y) = row_point(&surface, 0);

    // Leaving the row: off to the short row, then off the list.
    surface.mouse_move(x, y);
    settle(&mut surface);
    let (short_x, short_y) = row_point(&surface, 1);
    surface.mouse_move(short_x, short_y);
    let gone = Instant::now() + Duration::from_millis(500);
    surface.maintain_skill_tooltip(gone);
    assert!(!surface.skill_tooltip().is_present(), "left the row");

    // A key: at once; and the keyboard moving does not raise another.
    surface.mouse_move(x, y);
    settle(&mut surface);
    assert!(surface.skill_tooltip().is_present());
    assert!(press(&mut surface, NamedKey::ArrowDown));
    assert!(!surface.skill_tooltip().is_present(), "a key");
    assert!(press(&mut surface, NamedKey::ArrowUp));
    assert!(!surface.skill_tooltip().is_present(), "the keyboard's row");

    // The wheel over the list.
    surface.mouse_move(x + 1.0, y);
    settle(&mut surface);
    assert!(surface.skill_tooltip().is_present());
    assert!(surface.skill_scroll(x, y, MouseScrollDelta::LineDelta(0.0, -1.0)));
    assert!(!surface.skill_tooltip().is_present(), "a scroll");

    // The list closing, by a press outside it.
    surface.mouse_move(x, y);
    settle(&mut surface);
    assert!(surface.skill_tooltip().is_present());
    assert_eq!(surface.skill_mouse_down(1.0, 1.0), None);
    assert!(!surface.skill_list_open());
    assert!(!surface.skill_tooltip().is_present(), "the list closed");
}

/// **A queued message with a skill chip rides a send, never a steer** —
/// the running turn takes text alone.
#[test]
fn a_queued_message_with_a_skill_is_not_text_only() {
    let (mut surface, _commands) = surface();
    type_text(&mut surface, "Use $re");
    answer(&mut surface, listing());
    assert!(press(&mut surface, NamedKey::Enter));
    assert!(surface.queue_composer());
    let chat = surface.transcript.as_ref().unwrap().chat.id;
    let queue = surface.queues.get(chat).expect("queued");
    assert_eq!(queue.items[0].markdown, "Use [[koru://skill/review-pr]]");
    assert!(!queue.items[0].text_only());
}
