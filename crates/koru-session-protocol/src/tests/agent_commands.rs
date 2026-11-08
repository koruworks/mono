// SPDX-License-Identifier: MPL-2.0

use crate::*;

fn command(name: &str, kind: AgentCommandKind) -> AgentCommand {
    AgentCommand {
        name: name.into(),
        description: String::new(),
        display_name: None,
        kind,
        input_hint: None,
    }
}

/// **An entry's shape on the wire** (koru-ai D121): its kind in snake case,
/// and what it lacks left out, so an ACP command's input hint can join it
/// later without a new shape. `Debug` names no words.
#[test]
fn an_agent_command_says_its_kind_and_leaves_out_what_it_lacks() {
    let mut skill = command("review-pr", AgentCommandKind::RepoSkill);
    let json = serde_json::to_value(&skill).unwrap();
    assert_eq!(
        json,
        serde_json::json!({ "name": "review-pr", "kind": "repo_skill" })
    );
    skill.description = "Review the open pull request".into();
    skill.display_name = Some("Review PR".into());
    let json = serde_json::to_value(&skill).unwrap();
    assert_eq!(json["display_name"], "Review PR");
    assert_eq!(json["description"], "Review the open pull request");
    assert_eq!(serde_json::from_value::<AgentCommand>(json).unwrap(), skill);
    for (kind, word) in [
        (AgentCommandKind::UserSkill, "user_skill"),
        (AgentCommandKind::SystemSkill, "system_skill"),
        (AgentCommandKind::AdminSkill, "admin_skill"),
    ] {
        assert_eq!(serde_json::to_value(kind).unwrap(), word);
        assert!(kind.is_skill());
    }
    assert!(
        serde_json::from_value::<AgentCommandKind>(serde_json::json!("plugin")).is_err(),
        "a closed set"
    );
    let shown = format!("{skill:?}");
    assert!(
        !shown.contains("review") && !shown.contains("Review"),
        "{shown}"
    );
}

/// **A picker's order**: the folder's skills, the person's, then the
/// agent's own and an administrator's together; by name within each; a
/// name kept once, the first in that order; an invalid name never offered.
#[test]
fn commands_are_ordered_by_kind_then_name_and_kept_once() {
    let ordered = ordered_commands(vec![
        command("zeta", AgentCommandKind::SystemSkill),
        command("beta", AgentCommandKind::UserSkill),
        command("alpha", AgentCommandKind::AdminSkill),
        command("omega", AgentCommandKind::AdminSkill),
        command("shared", AgentCommandKind::UserSkill),
        command("shared", AgentCommandKind::RepoSkill),
        command("two words", AgentCommandKind::RepoSkill),
        command("", AgentCommandKind::RepoSkill),
        command("alpha", AgentCommandKind::RepoSkill),
    ]);
    let seen: Vec<(&str, AgentCommandKind)> = ordered
        .iter()
        .map(|command| (command.name.as_str(), command.kind))
        .collect();
    assert_eq!(
        seen,
        vec![
            ("alpha", AgentCommandKind::RepoSkill),
            ("shared", AgentCommandKind::RepoSkill),
            ("beta", AgentCommandKind::UserSkill),
            ("omega", AgentCommandKind::AdminSkill),
            ("zeta", AgentCommandKind::SystemSkill),
        ]
    );
    assert_eq!(
        AgentCommandKind::SystemSkill.group(),
        AgentCommandKind::AdminSkill.group()
    );
    let many = (0..MAX_AGENT_COMMANDS + 5)
        .map(|index| command(&format!("skill-{index:04}"), AgentCommandKind::UserSkill))
        .collect();
    assert_eq!(ordered_commands(many).len(), MAX_AGENT_COMMANDS);
}

/// A description is one line, cut, with no control characters; a name is
/// one token, never cut.
#[test]
fn names_are_one_token_and_text_is_one_line() {
    assert!(valid_command_name("pdf:pdf"));
    assert!(valid_command_name("my_skill-2"));
    assert!(!valid_command_name("two words"));
    assert!(!valid_command_name("tab\tname"));
    assert!(!valid_command_name(
        &"n".repeat(AGENT_COMMAND_NAME_BYTES + 1)
    ));
    assert_eq!(command_text("\n  First line.  \nSecond"), "First line.");
    assert_eq!(
        command_text(&"x".repeat(AGENT_COMMAND_TEXT_CHARS + 9))
            .chars()
            .count(),
        AGENT_COMMAND_TEXT_CHARS
    );
    assert_eq!(command_text("a\u{7}b"), "ab");
}

/// **An input's skills** (koru-ai D121): absent from the wire when there
/// are none — the shape every older peer reads — and bounded when there
/// are: valid names, none twice, at most [`MAX_INPUT_SKILLS`].
#[test]
fn an_inputs_skills_are_absent_when_none_and_bounded_when_named() {
    let mut input = PendingInput {
        model: None,
        idempotency_key: super::idempotency_key(),
        text: "Use $review-pr".into(),
        attachment_ids: Vec::new(),
        mode: None,
        fast: None,
        reasoning: None,
        skills: Vec::new(),
    };
    let json = serde_json::to_value(&input).unwrap();
    assert!(json.get("skills").is_none());
    assert!(!input.needs_agent_commands());
    input.skills = vec!["review-pr".into()];
    assert!(input.needs_agent_commands());
    let json = serde_json::to_value(&input).unwrap();
    assert_eq!(json["skills"], serde_json::json!(["review-pr"]));
    assert_eq!(serde_json::from_value::<PendingInput>(json).unwrap(), input);

    assert!(valid_input_skills(&[]));
    assert!(valid_input_skills(&["a".into(), "b".into()]));
    assert!(!valid_input_skills(&["a".into(), "a".into()]));
    assert!(!valid_input_skills(&["two words".into()]));
    let too_many: Vec<String> = (0..=MAX_INPUT_SKILLS)
        .map(|index| format!("s{index}"))
        .collect();
    assert!(!valid_input_skills(&too_many));
    assert!(capability::ALL.contains(&capability::AGENT_COMMANDS));
    assert_eq!(AGENT_COMMANDS_PROTOCOL_VERSION, 21);
}
