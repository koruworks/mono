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
