// SPDX-License-Identifier: MPL-2.0
use super::*;

fn create() -> Value {
    json!({"request_id":BotRequestId::mint(),"expected_bot_revision":1,"name":"Routine","objective":"Review work","documents":[],"recurrence":{"type":"floating","cron":"0 9 * * *"}})
}
#[test]
fn routine_tools_closed_arguments_cannot_select_authority_or_execution() {
    let read = json!({"request_id":BotRequestId::mint()});
    let retry = json!({"original_request_id":BotRequestId::mint(),"observed_token":{"state":"prepared","updated_at_ms":-1}});
    for (name, args) in [
        ("create_routine", create()),
        ("read_routine_creation", read),
        ("retry_routine_creation", retry),
        ("list_routines", json!({})),
    ] {
        assert!(parse(name, args.clone()).is_ok());
        for field in [
            "bot_id",
            "owner",
            "profile",
            "run",
            "generation",
            "model",
            "scope",
            "grant",
            "working_folder",
            "enabled",
            "execute",
            "operation_id",
        ] {
            let mut altered = args.clone();
            altered[field] = json!("injected");
            assert!(parse(name, altered).is_err(), "{name}/{field}");
        }
    }
}
#[test]
fn routine_tools_reject_noncanonical_ids_and_unknown_nested_fields() {
    for id in [
        "00000000-0000-0000-0000-000000000000",
        "aaaaaaaa-aaaa-5aaa-8aaa-aaaaaaaaaaaa",
        "AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA",
        "not-an-id",
    ] {
        assert!(parse("read_routine_creation", json!({"request_id":id})).is_err());
    }
    let mut args = create();
    args["recurrence"]["extra"] = json!(true);
    assert!(parse("create_routine", args).is_err());
    assert!(parse("retry_routine_creation",json!({"original_request_id":BotRequestId::mint(),"observed_token":{"state":"prepared","updated_at_ms":1,"extra":true}})).is_err());
}
#[test]
fn routine_tools_bound_arguments_and_preserve_ordering_only_cursor() {
    let cursor = ScheduleId::mint();
    let RunRoutineOperation::List {
        after_schedule,
        limit,
    } = parse("list_routines", json!({"after_schedule":cursor})).unwrap()
    else {
        panic!("list");
    };
    assert_eq!(after_schedule, Some(cursor));
    assert_eq!(limit, 16);
    for limit in [0, 17, 256] {
        assert!(parse("list_routines", json!({"limit":limit})).is_err());
    }
    let mut args = create();
    args["documents"] = json!((0..16).map(|_| DocumentId::new()).collect::<Vec<_>>());
    assert!(parse("create_routine", args.clone()).is_ok());
    args["documents"]
        .as_array_mut()
        .unwrap()
        .push(json!(DocumentId::new()));
    assert!(parse("create_routine", args).is_err());
    let mut args = create();
    args["objective"] = json!("x".repeat(4 * 1024 * 1024));
    let Err(error) = parse("create_routine", args) else {
        panic!("bounded arguments");
    };
    assert_eq!(error.code, "context_too_large");
}
#[test]
fn routine_tools_catalog_has_exact_closed_schemas_and_safe_annotations() {
    let tools = definitions();
    assert_eq!(tools.len(), 4);
    let names = tools.iter().map(|t| t.name.as_ref()).collect::<Vec<_>>();
    assert_eq!(
        names,
        vec![
            "create_routine",
            "read_routine_creation",
            "retry_routine_creation",
            "list_routines"
        ]
    );
    for tool in &tools {
        let wire = serde_json::to_value(tool).unwrap();
        assert_eq!(wire["inputSchema"]["additionalProperties"], false);
        assert_eq!(wire["annotations"]["destructiveHint"], false);
        assert_eq!(wire["annotations"]["openWorldHint"], false);
        assert_eq!(wire["annotations"]["readOnlyHint"], !mutation(&tool.name));
    }
    assert_eq!(
        serde_json::to_value(&tools[0]).unwrap()["inputSchema"]["properties"]["documents"]["maxItems"],
        16
    );
    let schema = serde_json::to_value(&tools[0]).unwrap();
    assert_eq!(
        schema["inputSchema"]["properties"]["expected_bot_revision"]["minimum"],
        1
    );
    assert_eq!(
        schema["inputSchema"]["properties"]["expected_bot_revision"]["maximum"],
        json!(i64::MAX)
    );
    assert!(!recognizes("enable_routine"));
    assert!(!recognizes("run_routine"));
}
#[test]
fn routine_tools_async_guard_cancels_without_owning_physical_completion() {
    let cancellation = Cancellation::default();
    let still_owned = cancellation.clone();
    {
        let _guard = CancelOnDrop(cancellation);
    }
    assert!(still_owned.is_cancelled());
    // Actual surviving physical capacity is covered by the spawned AI worker
    // held-phase test; this checks only the future-owned cancellation primitive.
}

#[cfg(windows)]
mod composition;

/// P4.3: no owner control of a Routine is a model tool. A Run can set a
/// Routine up and list Routines; it can never turn one on, pause it, test
/// it, edit it or change its access, under any spelling, and no tool takes
/// an argument that would.
#[test]
fn routine_tools_offer_no_owner_control_of_a_routine() {
    for name in [
        "set_routine_lifecycle",
        "enable_routine",
        "pause_routine",
        "resume_routine",
        "test_routine",
        "run_routine",
        "edit_routine",
        "rename_routine",
        "update_routine_access",
        "read_routine_detail",
        "delete_routine",
        "archive",
        "trash",
        "restore",
    ] {
        assert!(!recognizes(name), "{name}");
        assert!(parse(name, json!({})).is_err(), "{name}");
        assert!(definitions().iter().all(|tool| tool.name != name), "{name}");
    }
    // The four tools that do exist cannot be steered into a control.
    for (name, args) in [("create_routine", create()), ("list_routines", json!({}))] {
        for field in [
            "action",
            "lifecycle",
            "state",
            "expected_definition_revision",
            "expected_grant_revision",
            "access",
            "selected_pages",
            "schedule_id",
            "occurrence_ms",
        ] {
            let mut altered = args.clone();
            altered[field] = json!("enable");
            assert!(parse(name, altered).is_err(), "{name}/{field}");
        }
    }
    // Every owner control is a Bot command of the vouched owner route only.
    for command in [
        "set_routine_lifecycle",
        "test_routine",
        "edit_routine",
        "update_routine_access",
    ] {
        let schema = serde_json::to_value(schemars::schema_for!(koru_ai_wire::BotCommand)).unwrap();
        assert!(schema.to_string().contains(command), "{command}");
    }
}
