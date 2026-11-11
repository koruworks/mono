// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_draw::brand::ProfileMark;
use koru_draw::icons;

fn agent(id: &str, key: &str, label: &str) -> CatalogAgent {
    CatalogAgent {
        id: AiProviderId::parse(id).unwrap(),
        key: Some(key.into()),
        label: label.into(),
        mark: ProfileMark::Glyph(icons::ROBOT),
    }
}

fn id(text: &str) -> AiProviderId {
    AiProviderId::parse(text).unwrap()
}

/// What the link was asked, in order, as words.
fn asked(commands: &Receiver<Command>) -> Vec<String> {
    let mut out = Vec::new();
    while let Ok(command) = commands.try_recv() {
        out.push(match command {
            Command::Agents { seq } => format!("agents {seq}"),
            Command::Readiness { seq, agent } => format!("readiness {seq} {agent}"),
            Command::Listing {
                seq,
                agent,
                refresh,
            } => format!("listing {seq} {agent} {refresh}"),
        });
    }
    out
}

/// **Nothing is asked before the catalog is shown, nothing while it is
/// hidden; shown, it asks for the agents, then each one's readiness.**
#[test]
fn the_catalog_asks_when_shown_and_then_for_each_agents_readiness() {
    let (link, commands, answers) = CatalogLink::test_pair();
    let mut catalog = Catalog::with_link(link);
    assert!(!catalog.pass(None));
    assert!(
        asked(&commands).is_empty(),
        "nothing owed before it is shown"
    );
    catalog.shown();
    catalog.pass(None);
    assert_eq!(asked(&commands), ["agents 1"]);
    assert_eq!(catalog.sent(), 1);
    answers
        .send(Answer::Agents {
            seq: 1,
            result: Ok(vec![
                agent("agent-codex", "codex", "Codex"),
                agent("agent-claude", "claude", "Claude"),
            ]),
        })
        .unwrap();
    assert!(catalog.pass(None));
    assert_eq!(catalog.agents().len(), 2);
    // One readiness read an agent, in whichever order the set gives them.
    let mut readiness: Vec<String> = asked(&commands)
        .into_iter()
        .map(|line| line.split(' ').nth(2).unwrap().to_owned())
        .collect();
    readiness.sort();
    assert_eq!(readiness, ["agent-claude", "agent-codex"]);
    assert!(catalog.waiting());
}

/// **Only the latest answer is taken, and a failed read keeps what was
/// shown**: an older agents answer is dropped, a refusal leaves the agents
/// as they were, a listing that does not come keeps the listing shown.
#[test]
fn a_stale_or_failed_answer_keeps_what_was_shown() {
    let (link, commands, answers) = CatalogLink::test_pair();
    let mut catalog = Catalog::with_link(link);
    catalog.shown();
    catalog.pass(None);
    asked(&commands);
    answers
        .send(Answer::Agents {
            seq: 1,
            result: Ok(vec![agent("agent-codex", "codex", "Codex")]),
        })
        .unwrap();
    catalog.pass(None);
    asked(&commands);
    // Shown again: the agents are read again, and the read fails.
    catalog.shown();
    catalog.pass(None);
    let again = asked(&commands);
    assert_eq!(again.len(), 1, "{again:?}");
    let seq: u64 = again[0].trim_start_matches("agents ").parse().unwrap();
    answers
        .send(Answer::Agents {
            seq: seq - 1,
            result: Ok(Vec::new()),
        })
        .unwrap();
    answers
        .send(Answer::Agents {
            seq,
            result: Err(Failure::lost()),
        })
        .unwrap();
    catalog.pass(None);
    assert_eq!(
        catalog.agents().len(),
        1,
        "kept through a stale and a failed read"
    );

    // A listing, then a ↻ that does not come back: the listing stays.
    let codex = id("agent-codex");
    catalog.want(&codex);
    catalog.pass(None);
    let listing = asked(&commands);
    let seq: u64 = listing[0].split(' ').nth(1).unwrap().parse().unwrap();
    answers
        .send(Answer::Listing {
            seq,
            agent: codex.clone(),
            result: Ok(CatalogListing::default()),
        })
        .unwrap();
    catalog.pass(None);
    assert!(catalog.listing(&codex).is_some());
    // Wanted again while listed: nothing is asked.
    catalog.want(&codex);
    catalog.pass(None);
    assert!(asked(&commands).is_empty());
    catalog.refresh(&codex);
    catalog.pass(None);
    let refresh = asked(&commands);
    assert!(refresh[0].ends_with("agent-codex true"), "{refresh:?}");
    let seq: u64 = refresh[0].split(' ').nth(1).unwrap().parse().unwrap();
    answers
        .send(Answer::Listing {
            seq,
            agent: codex.clone(),
            result: Err(ListingFailure::Unreachable),
        })
        .unwrap();
    catalog.pass(None);
    assert!(
        catalog.listing(&codex).is_some(),
        "the listing shown is kept"
    );
    assert_eq!(catalog.take_refreshed(), [codex.clone(), codex]);
}

/// **The control's agents**: every registered runtime, in the person's
/// order, turned-off ones marked, readiness and listing as known.
#[test]
fn the_controls_agents_follow_the_persons_order_and_switches() {
    let (link, _commands, answers) = CatalogLink::test_pair();
    let mut catalog = Catalog::with_link(link);
    catalog.shown();
    catalog.pass(None);
    answers
        .send(Answer::Agents {
            seq: 1,
            result: Ok(vec![
                agent("agent-claude", "claude", "Claude"),
                agent("agent-codex", "codex", "Codex"),
                agent("agent-opencode", "opencode", "OpenCode"),
            ]),
        })
        .unwrap();
    catalog.pass(None);
    answers
        .send(Answer::Readiness {
            seq: catalog.readiness_seq[&id("agent-codex")],
            agent: id("agent-codex"),
            result: Ok(Readiness::Ready),
        })
        .unwrap();
    catalog.pass(None);
    let settings = AgentSettings {
        hidden: vec!["claude".into()],
        order: vec!["codex".into(), "opencode".into(), "claude".into()],
        favorites: Vec::new(),
        default_agent: None,
    };
    let agents = catalog.control_agents(&settings);
    assert_eq!(
        agents
            .iter()
            .map(|agent| (agent.label, agent.hidden, agent.readiness))
            .collect::<Vec<_>>(),
        [
            ("Codex", false, Some(Readiness::Ready)),
            ("OpenCode", false, None),
            ("Claude", true, None),
        ]
    );
    // A new Bot starts on the ready one, never a turned-off default.
    let mut with_default = settings.clone();
    with_default.default_agent = Some("claude".into());
    assert_eq!(catalog.preferred(&with_default), Some(id("agent-codex")));
    with_default.default_agent = Some("opencode".into());
    assert_eq!(catalog.preferred(&with_default), Some(id("agent-opencode")));
}

/// Answer every readiness read owed with `result` for `agent`; how many.
fn answer_readiness(
    commands: &Receiver<Command>,
    answers: &SyncSender<Answer>,
    result: Result<Readiness, Failure>,
) -> usize {
    let mut n = 0;
    while let Ok(command) = commands.try_recv() {
        if let Command::Readiness { seq, agent } = command {
            answers
                .send(Answer::Readiness {
                    seq,
                    agent,
                    result: result.clone(),
                })
                .unwrap();
            n += 1;
        }
    }
    n
}

/// **A readiness still being checked ends in a sentence, never a spinner
/// forever** (the first window run's finding 3: *Checking…* for more than
/// two minutes on a machine where nothing can run). An agent answering
/// *checking* — as `validate_provider` answers while its probe re-runs
/// (`agent_checking`, `koru_ai_ui::agents::AgentStatus::from_validation`) —
/// or not answering is asked again on the bounded clock; past
/// `CHECKING_LIMIT` the catalog stops asking and says it could not tell:
/// the control is told `CheckFailed`, the card `Standing::Unknown`, and the
/// form, with no agent able to run, refuses before anything is sent. A
/// settled answer later, or the form shown again, puts that right.
#[test]
fn a_readiness_still_being_checked_is_asked_again_and_then_said_unknown() {
    let (link, commands, answers) = CatalogLink::test_pair();
    let mut catalog = Catalog::with_link(link);
    let settings = AgentSettings::default();
    let start = std::time::Instant::now();
    catalog.shown();
    catalog.pass_at(None, start);
    asked(&commands);
    answers
        .send(Answer::Agents {
            seq: 1,
            result: Ok(vec![agent("agent-claude", "claude", "Claude")]),
        })
        .unwrap();
    catalog.pass_at(None, start);
    let claude = id("agent-claude");
    assert_eq!(
        catalog.standing(&claude),
        Standing::Checking,
        "not answered yet"
    );
    assert!(!catalog.none_can_run(&settings), "nothing known yet");
    assert_eq!(
        answer_readiness(&commands, &answers, Ok(Readiness::Checking)),
        1
    );
    catalog.pass_at(None, start);
    assert_eq!(catalog.standing(&claude), Standing::Checking);
    let due = catalog.deadline().expect("asked again on a clock");
    assert!(due > start, "{due:?}");
    // Due: asked again. Unanswered this time: still checking.
    catalog.pass_at(None, due);
    assert_eq!(
        answer_readiness(&commands, &answers, Err(Failure::lost())),
        1,
        "asked again when due"
    );
    catalog.pass_at(None, due);
    assert_eq!(catalog.standing(&claude), Standing::Checking);
    // Past the limit: no more asking, and it says it could not tell.
    let late = start + CHECKING_LIMIT + std::time::Duration::from_secs(1);
    catalog.pass_at(None, late);
    answer_readiness(&commands, &answers, Ok(Readiness::Checking));
    catalog.pass_at(None, late);
    assert_eq!(catalog.standing(&claude), Standing::Unknown);
    assert_eq!(catalog.readiness(&claude), Some(Readiness::CheckFailed));
    assert_eq!(catalog.deadline(), None, "the clock stops");
    assert!(catalog.none_can_run(&settings), "no agent offered can run");
    catalog.pass_at(None, late + std::time::Duration::from_secs(60));
    assert!(asked(&commands).is_empty(), "nothing asked after the limit");
    // Shown again: asked afresh, and a settled answer is what it says.
    catalog.shown();
    catalog.pass_at(None, late);
    let seq: u64 = asked(&commands)[0]
        .trim_start_matches("agents ")
        .parse()
        .unwrap();
    answers
        .send(Answer::Agents {
            seq,
            result: Ok(vec![agent("agent-claude", "claude", "Claude")]),
        })
        .unwrap();
    catalog.pass_at(None, late);
    assert_eq!(catalog.standing(&claude), Standing::Checking);
    answer_readiness(&commands, &answers, Ok(Readiness::NotInstalled));
    catalog.pass_at(None, late);
    assert_eq!(
        catalog.standing(&claude),
        Standing::NotReady(Readiness::NotInstalled)
    );
    assert_eq!(catalog.deadline(), None, "settled: nothing more to ask");
    assert!(catalog.none_can_run(&settings));
}
