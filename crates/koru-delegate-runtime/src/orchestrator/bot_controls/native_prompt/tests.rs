// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn bot_input_preserves_role_history_and_task_without_artifact_instructions() {
    let run = RunId::mint();
    let root = Path::new("isolated-bot-work");
    let input = "Role: researcher\nHistory: prior question\nTask: explain the finding";
    let composed = compose(run, root, input);
    assert!(composed.ends_with(input));
    assert_eq!(composed.matches(input).count(), 1);
    for tool in [
        "search_documents",
        "read_document",
        "create_document",
        "edit_document",
    ] {
        assert!(composed.contains(tool));
    }
    assert!(composed.contains(&run.to_string()));
    assert!(composed.contains("status saved"));
    assert!(!composed.contains(".artifacts"));
    assert!(!composed.contains("Nobody reads this conversation"));
    let ordinary = crate::orchestrator::start::compose_first_submission(
        "An edited ordinary preamble",
        run,
        root,
        input,
    );
    assert!(ordinary.starts_with("An edited ordinary preamble\n\n"));
    assert!(ordinary.contains(".artifacts"));
    assert!(ordinary.ends_with(input));
}
