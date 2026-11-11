// SPDX-License-Identifier: MPL-2.0

//! With the `schema` feature on, every sync and execution shape describes
//! itself — the API plan that publishes `/v1/runs` generates its document
//! from these types rather than from a second declaration.

use super::*;

#[test]
fn every_top_level_shape_has_a_schema() {
    let run = schemars::schema_for!(DelegatedRun);
    let json = serde_json::to_value(&run).unwrap();
    let definitions = json["$defs"].as_object().unwrap();
    for named in [
        "RunHeader",
        "RunStatus",
        "RunContextItem",
        "WorkspaceScope",
        "ExecutionEnvironment",
        "TimelineEntry",
        "StepKind",
        "EconomicsSummary",
        "RunRating",
        "ResultReference",
        "RunId",
        "ArtifactVersionId",
        "DocumentVersionId",
    ] {
        assert!(
            definitions.contains_key(named),
            "{named} missing from the schema"
        );
    }
    let _ = schemars::schema_for!(AttemptState);
    let _ = schemars::schema_for!(DelegationRequest);
    let _ = schemars::schema_for!(UsageFact);
    let _ = schemars::schema_for!(ModelIdentity);
    let _ = schemars::schema_for!(RunEvent);
    // The Run view `/v1/runs` and the Bots API's `read_run` answer.
    let view = serde_json::to_value(schemars::schema_for!(RunView)).unwrap();
    for named in [
        "RunExecution",
        "TimelineWindow",
        "TimelineItem",
        "RunHeader",
    ] {
        assert!(
            view["$defs"].get(named).is_some(),
            "{named} missing from RunView"
        );
    }
}
