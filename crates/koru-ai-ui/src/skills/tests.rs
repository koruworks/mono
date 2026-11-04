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
use crate::composer::dollar_before;
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

/// **`$` asks the agent and opens when it answers**: the chat's agent and
/// folder are asked once; the list opens, filtered, as the person types,
/// and shuts when nothing matches or the caret leaves the token.
#[test]
fn a_dollar_asks_for_the_list_and_opens_on_the_answer() {
    let (mut surface, commands) = surface();
    type_text(&mut surface, "Use $");
    assert!(
        !surface.skill_list_open(),
        "nothing listed yet: just a character"
    );
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
    assert!(!surface.skill_list_open(), "nothing matches");
    type_text(&mut surface, "Use $pr and");
    assert!(!surface.skill_list_open(), "the caret left the token");
    assert!(surface.skill_panel().is_none());
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
    assert_eq!(message.markdown, "Use $release-notes ");
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
    assert_eq!(request.markdown, "Use $review-pr");
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

/// **No list, no list**: a chat whose agent lists no skills, and a chat
/// with no local agent, keep `$` a character — the second never asks.
#[test]
fn without_skills_a_dollar_is_only_a_character() {
    let (mut surface, commands) = surface();
    type_text(&mut surface, "$");
    answer(&mut surface, Vec::new());
    assert!(!surface.skill_list_open());
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
    assert_eq!(queue.items[0].markdown, "Use $review-pr");
    assert!(!queue.items[0].text_only());
}
