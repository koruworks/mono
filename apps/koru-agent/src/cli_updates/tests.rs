// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_cli_updates::CliReport;

#[test]
fn the_wire_report_is_the_controller_s_report_stage_for_stage() {
    let report = Report {
        checked: true,
        clis: vec![
            CliReport {
                id: "codex".into(),
                name: "Codex".into(),
                installed: "0.163.0".into(),
                latest: "0.165.0".into(),
                stage: CliStage::Available,
                message: None,
                detail: None,
            },
            CliReport {
                id: "grok".into(),
                name: "Grok".into(),
                installed: "1.0.60".into(),
                latest: "1.0.70".into(),
                stage: CliStage::Updating,
                message: Some("Downloading".into()),
                detail: None,
            },
        ],
    };
    let wire = report_for(&report);
    assert!(wire.checked);
    assert_eq!(wire.clis.len(), 2);
    assert_eq!(wire.clis[0].stage, AgentCliStage::Available);
    assert_eq!(wire.clis[1].stage, AgentCliStage::Updating);
    assert_eq!(wire.clis[1].message.as_deref(), Some("Downloading"));
    assert_eq!(report_for(&Report::default()), AgentCliReport::default());
}

#[test]
fn a_choice_about_a_cli_the_check_never_found_is_refused_with_its_code() {
    let controller = Controller::detached();
    let refused = apply(&controller, &AgentCliChoice::Update { id: "codex".into() }).unwrap_err();
    assert_eq!(refused.0, "no_such_cli");
    let refused = apply(&controller, &AgentCliChoice::Dismiss { id: "codex".into() }).unwrap_err();
    assert_eq!(refused.0, "no_such_cli");
}

fn row(id: &str, installed: &str, stage: AgentCliStage) -> AgentCliRow {
    AgentCliRow {
        id: id.into(),
        name: id.into(),
        installed: installed.into(),
        latest: String::new(),
        stage,
        message: None,
        detail: None,
    }
}

fn checked(clis: Vec<AgentCliRow>) -> AgentCliReport {
    AgentCliReport {
        checked: true,
        clis,
    }
}

fn kinds(targets: &[(String, ProviderKind)]) -> Vec<&str> {
    targets.iter().map(|(_, kind)| kind.as_str()).collect()
}

/// Agent-models D6: a CLI updated from its card, or moved under Koru's
/// feet, re-probes the agent that runs it; nothing else does.
#[test]
fn an_updated_cli_refreshes_the_agent_that_runs_it_and_nothing_else() {
    use AgentCliStage::*;
    let before = checked(vec![
        row("opencode", "1.18.0", Updating),
        row("kimi", "0.42.0", UpToDate),
        row("grok", "1.0.25", UpToDate),
        row("claude", "2.1.0", Available),
        row("copilot", "1.0.0", Available),
        row("qwen", "0.1.0", Available),
    ]);
    let after = checked(vec![
        row("opencode", "1.19.5", Updated),
        row("kimi", "0.43.0", UpToDate),
        row("grok", "1.0.25", UpToDate),
        row("claude", "2.2.0", Updated),
        row("copilot", "1.1.0", Updated),
        row("qwen", "0.2.0", UpToDate),
    ]);
    let targets = refresh_targets(Some(&before), &after);
    assert_eq!(
        kinds(&targets),
        ["opencode", "kimi"],
        "an Updated stage and an installed change each name their agent; \
         claude's route runs its own pinned wrapper, copilot and qwen no route"
    );
    assert_eq!(targets[0].0, "opencode");

    // An unchanged report, and a report with nothing to compare against,
    // refresh nothing: the first check after a start is not a change.
    assert!(refresh_targets(Some(&after), &after).is_empty());
    assert!(refresh_targets(None, &after).is_empty());
    let unchecked = AgentCliReport::default();
    assert!(refresh_targets(Some(&unchecked), &after).is_empty());
    // A card that stays Updated across reports is not a second update.
    let still = checked(vec![row("opencode", "1.19.5", Updated)]);
    assert!(refresh_targets(Some(&still), &still).is_empty());
    // An agent named twice is refreshed once.
    let codex_before = checked(vec![row("codex", "0.150.0", Updating)]);
    let codex_after = checked(vec![row("codex", "0.153.4", Updated)]);
    assert_eq!(
        kinds(&refresh_targets(Some(&codex_before), &codex_after)),
        ["codex"]
    );
}
