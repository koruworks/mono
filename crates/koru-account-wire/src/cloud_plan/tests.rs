// SPDX-License-Identifier: MPL-2.0
use super::*;
use serde_json::{Value, json};

fn fixtures() -> Vec<Value> {
    serde_json::from_str::<Value>(include_str!("../cloud_plan.fixtures.json")).unwrap()["fixtures"]
        .as_array()
        .unwrap()
        .clone()
}

#[test]
fn all_approved_fixtures_round_trip_without_losing_nulls() {
    for fixture in fixtures() {
        let response = &fixture["response"];
        let actual = if response["contract"] == "cloud-plan-v1" {
            serde_json::to_value(
                serde_json::from_value::<CloudPlanResponseV1>(response.clone()).unwrap(),
            )
            .unwrap()
        } else {
            serde_json::to_value(
                serde_json::from_value::<CloudUsageWorkspacesResponseV1>(response.clone()).unwrap(),
            )
            .unwrap()
        };
        assert_eq!(&actual, response, "{}", fixture["name"]);
    }
}

#[test]
fn workspace_page_rejects_duplicate_identity_even_with_a_different_incarnation() {
    let page = fixtures()
        .into_iter()
        .find(|f| f["name"] == "workspace_page")
        .unwrap()["response"]
        .clone();
    for changed_incarnation in [false, true] {
        let mut invalid = page.clone();
        let mut duplicate = invalid["items"][0].clone();
        if changed_incarnation {
            duplicate["incarnation_id"] = json!("00000000-0000-0000-0000-000000000099");
        }
        invalid["items"].as_array_mut().unwrap().push(duplicate);
        assert!(serde_json::from_value::<CloudUsageWorkspacesResponseV1>(invalid).is_err());
    }
}

#[test]
fn bigint_is_exact_and_canonical() {
    for value in [0, 9_007_199_254_740_993, i64::MAX] {
        let decimal = Decimal::new(value).unwrap();
        let json = serde_json::to_value(decimal).unwrap();
        assert_eq!(json, Value::String(value.to_string()));
        assert_eq!(
            serde_json::from_value::<Decimal>(json).unwrap().get(),
            value
        );
    }
    for value in [
        json!(0),
        json!(-1),
        json!(null),
        json!("-1"),
        json!("+1"),
        json!("01"),
        json!("1.0"),
        json!("9223372036854775808"),
        json!(""),
    ] {
        assert!(
            serde_json::from_value::<Decimal>(value.clone()).is_err(),
            "{value}"
        );
    }
}

#[test]
fn malformed_and_future_states_cannot_become_false_success() {
    let original = fixtures()[0]["response"].clone();
    for path in ["tier", "mcp_allowed"] {
        let mut value = original.clone();
        value["policy"][path] = json!("future");
        assert!(serde_json::from_value::<CloudPlanResponseV1>(value).is_err());
    }
    for path in ["reason", "download_until_millis", "removal_at_millis"] {
        let mut value = original.clone();
        value["policy"]["access"]
            .as_object_mut()
            .unwrap()
            .remove(path);
        assert!(serde_json::from_value::<CloudPlanResponseV1>(value).is_err());
    }
    for path in ["generation", "reason", "available_bytes"] {
        let mut value = original.clone();
        value["usage"].as_object_mut().unwrap().remove(path);
        assert!(serde_json::from_value::<CloudPlanResponseV1>(value).is_err());
    }
    let mut value = original.clone();
    value["usage"]["used_bytes"] = json!("400000001");
    assert!(serde_json::from_value::<CloudPlanResponseV1>(value).is_err());
    let mut value = original.clone();
    value["usage"]["last_known"] = json!({});
    assert!(serde_json::from_value::<CloudPlanResponseV1>(value).is_err());
    let mut value = original.clone();
    value["policy"]["purchase"]["available_intervals"] = json!(["year", "year"]);
    assert!(serde_json::from_value::<CloudPlanResponseV1>(value).is_err());
    let mut value = original;
    value["usage"]["algorithm"] = json!("files-encrypted-v1");
    assert!(serde_json::from_value::<CloudPlanResponseV1>(value).is_err());
}

#[test]
fn unknown_and_unassigned_placement_keep_required_null() {
    assert!(serde_json::from_value::<PlacementV1>(json!({"state":"unknown"})).is_err());
    assert!(
        serde_json::from_value::<PlacementV1>(json!({"state":"assigned","profile_id":null}))
            .is_err()
    );
    for state in ["unknown", "unassigned"] {
        let value = json!({"state":state,"profile_id":null});
        assert_eq!(
            serde_json::to_value(serde_json::from_value::<PlacementV1>(value.clone()).unwrap())
                .unwrap(),
            value
        );
    }
}

#[test]
fn free_wire_is_finite_and_carries_cloud_mcp() {
    let policy = PolicyV1::try_from(&CloudPlanPolicy::fake_free()).unwrap();
    policy.validate().unwrap();
    let json = serde_json::to_value(policy).unwrap();
    assert_eq!(json["tier"], "free");
    assert_eq!(json["allowance"]["effective_bytes"], "1000000000");
    assert_eq!(json["mcp_allowed"], true);
    assert_eq!(json["access"]["state"], "read_write");
}

#[test]
fn workspace_diagnostics_do_not_disclose_names_or_identifiers() {
    let page: CloudUsageWorkspacesResponseV1 =
        serde_json::from_value(fixtures()[7]["response"].clone()).unwrap();
    assert_eq!(
        format!("{page:?}"),
        "CloudUsageWorkspacesResponseV1(<redacted>)"
    );
    assert_eq!(
        format!("{:?}", page.items[0]),
        "WorkspaceUsageItemV1(<redacted>)"
    );
}
