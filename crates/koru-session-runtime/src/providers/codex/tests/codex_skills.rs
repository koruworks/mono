// SPDX-License-Identifier: MPL-2.0

//! **Codex's skills** (koru-ai D121): `skills/list` in the shape 0.166.0's
//! generated schema gives it, to the picker's list, and a picked skill to
//! the `skill` input item beside the text.

use koru_session_protocol::{AgentCommand, AgentCommandKind};

use super::*;

const LISTING: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/codex-app-server/0.166.0/skills-list.result.json"
));

fn listing() -> Value {
    serde_json::from_str(LISTING).unwrap()
}

fn cwd() -> String {
    std::env::current_dir()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

fn names(commands: &[AgentCommand]) -> Vec<(&str, AgentCommandKind)> {
    commands
        .iter()
        .map(|command| (command.name.as_str(), command.kind))
        .collect()
}

/// **The fixture becomes the picker's list**: enabled skills only, the
/// folder's first, then the person's, then the agent's own and an
/// administrator's together, by name within each; the folder's `review-pr` stands
/// for the person's; each description one line, the interface's short line
/// first; a display name only where it says more than the name.
#[test]
fn the_listing_becomes_the_pickers_list() {
    let listed = skills::listing(&listing()).unwrap();
    let commands: Vec<AgentCommand> = listed.iter().map(|skill| skill.command.clone()).collect();
    assert_eq!(
        names(&commands),
        vec![
            ("review-pr", AgentCommandKind::RepoSkill),
            ("docs:docs", AgentCommandKind::UserSkill),
            ("release-notes", AgentCommandKind::UserSkill),
            ("org-policy", AgentCommandKind::AdminSkill),
            ("skill-creator", AgentCommandKind::SystemSkill),
        ]
    );
    assert_eq!(
        commands[0].description,
        "Review the open pull request against the repository's rules."
    );
    assert_eq!(commands[0].display_name, None);
    assert_eq!(commands[2].description, "Write the notes for a release");
    assert_eq!(commands[2].display_name.as_deref(), Some("Release Notes"));
    assert_eq!(commands[1].display_name.as_deref(), Some("Docs"));
    assert_eq!(commands[3].description, "");
    assert_eq!(commands[4].description, "Create or update a skill.");
    assert_eq!(listed[0].path, "/fixture/repo/skills/review-pr/SKILL.md");
    assert!(skills::listing(&json!({ "nothing": [] })).is_none());
}

/// **Discovery starts an app-server, lists the folder and shuts it down**:
/// `initialize`, `initialized`, `skills/list` for the folder and nothing
/// else — no thread, no prompt.
#[test]
fn discovery_lists_the_folder_and_opens_no_thread() {
    let (mut control, state) = replay_control(
        [
            initialize_frames(false).remove(0),
            json!({ "id": 2, "result": listing() }),
        ],
        Arc::new(NoopAttachmentPathResolver),
    );
    let commands = control.discover_commands().unwrap();
    assert_eq!(commands.len(), 5);
    let state = state.lock().unwrap();
    let methods: Vec<&str> = state
        .sent
        .iter()
        .filter_map(|message| message["method"].as_str())
        .collect();
    assert_eq!(methods, ["initialize", "initialized", "skills/list"]);
    assert_eq!(state.sent[2]["params"], json!({ "cwds": [cwd()] }));
    assert_eq!(state.shutdowns, 1);
}

fn started(frames: Vec<Value>) -> (CodexControl, Arc<Mutex<ReplayState>>) {
    let mut inbound = initialize_frames(false);
    inbound.extend(frames);
    let (mut control, state) = replay_control(inbound, Arc::new(NoopAttachmentPathResolver));
    control
        .start(AgentStart {
            session_id: SessionId::mint(),
            parent_session_id: None,
            resume: None,
        })
        .unwrap();
    (control, state)
}

fn input(text: &str, skills: &[&str]) -> AgentInput {
    AgentInput {
        model: None,
        turn_id: AgentTurnId::mint(),
        text: text.into(),
        attachments: Vec::new(),
        mode: None,
        fast: None,
        reasoning: None,
        skills: skills.iter().map(|name| (*name).to_owned()).collect(),
    }
}

fn turn_inputs(state: &Arc<Mutex<ReplayState>>) -> Vec<Value> {
    state
        .lock()
        .unwrap()
        .sent
        .iter()
        .filter(|message| message["method"] == "turn/start")
        .map(|message| message["params"]["input"].clone())
        .collect()
}

fn listings(state: &Arc<Mutex<ReplayState>>) -> usize {
    state
        .lock()
        .unwrap()
        .sent
        .iter()
        .filter(|message| message["method"] == "skills/list")
        .count()
}

/// **A picked skill rides beside the text**: the text says `$name`, and the
/// `skill` item names it with the path from the session's listing — read
/// at the first turn that names one, kept for the next, and read again
/// after `skills/changed`. A name no longer listed is left out and the text
/// goes as typed.
#[test]
fn a_turn_names_its_skills_beside_the_text_from_the_last_listing() {
    let mut changed = listing();
    changed["data"][0]["skills"]
        .as_array_mut()
        .unwrap()
        .retain(|skill| skill["name"] != "review-pr");
    let (mut control, state) = started(vec![
        json!({ "id": 4, "result": listing() }),
        turn_started(5, "native-1"),
        json!({ "method": "skills/changed", "params": {} }),
        turn_started(6, "native-2"),
        json!({ "id": 7, "result": changed }),
        turn_started(8, "native-3"),
    ]);
    // A plain message lists nothing.
    assert_eq!(listings(&state), 0);

    control
        .submit(input("Use $review-pr and $gone", &["review-pr", "gone"]))
        .unwrap();
    control
        .submit(input("Again $review-pr", &["review-pr"]))
        .unwrap();
    assert_eq!(listings(&state), 1, "the listing is kept for the next turn");
    control
        .submit(input("Once more $review-pr", &["review-pr"]))
        .unwrap();
    assert_eq!(listings(&state), 2, "read again after skills/changed");

    let inputs = turn_inputs(&state);
    let skill = json!({
        "type": "skill",
        "name": "review-pr",
        "path": "/fixture/repo/skills/review-pr/SKILL.md"
    });
    assert_eq!(
        inputs[0],
        json!([{ "type": "text", "text": "Use $review-pr and $gone" }, skill])
    );
    assert_eq!(
        inputs[1],
        json!([{ "type": "text", "text": "Again $review-pr" }, skill])
    );
    assert_eq!(
        inputs[2],
        json!([{ "type": "text", "text": "Once more $review-pr" }]),
        "vanished: the text goes as typed"
    );
}

/// A listing that fails sends the text as typed; the turn still starts.
#[test]
fn a_failed_listing_sends_the_text_as_typed() {
    let (mut control, state) = started(vec![
        json!({ "id": 4, "error": { "code": -32603, "message": "fixture" } }),
        turn_started(5, "native-1"),
    ]);
    control
        .submit(input("Use $review-pr", &["review-pr"]))
        .unwrap();
    assert_eq!(
        turn_inputs(&state),
        vec![json!([{ "type": "text", "text": "Use $review-pr" }])]
    );
}

/// **A steer carries text alone**: one that names a skill is refused, and
/// nothing is sent.
#[test]
fn a_steer_never_carries_a_skill() {
    let (mut control, state) = started(vec![turn_started(4, "native-1")]);
    let first = input("first", &[]);
    let turn_id = first.turn_id;
    control.submit(first).unwrap();
    let mut steer = input("and $review-pr", &["review-pr"]);
    steer.turn_id = turn_id;
    let refused = control.steer(steer).unwrap_err();
    assert_eq!(refused.code, ErrorCode::DeliveryUnsupported);
    assert!(
        state
            .lock()
            .unwrap()
            .sent
            .iter()
            .all(|message| message["method"] != "turn/steer"),
        "nothing steered"
    );
    assert_eq!(listings(&state), 0);
}

/// A turn's skills are part of its fingerprint: the same turn naming other
/// skills is a conflict, never a replay.
#[test]
fn the_same_turn_with_other_skills_is_a_conflict() {
    let (mut control, _state) = started(vec![
        json!({ "id": 4, "result": listing() }),
        turn_started(5, "native-1"),
    ]);
    let first = input("Use $review-pr", &["review-pr"]);
    control.submit(first.clone()).unwrap();
    let mut other = first;
    other.skills = vec!["release-notes".into()];
    assert_eq!(
        control.submit(other).unwrap_err().code,
        ErrorCode::IdempotencyConflict
    );
}
