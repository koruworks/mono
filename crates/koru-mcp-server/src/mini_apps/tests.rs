// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn mini_app_published_schema_preserves_every_normative_branch_and_matches_validator() {
    use koru_ai_core::mini_apps::{self, Declaration};
    let published = tools();
    for definition in mini_apps::definitions() {
        let tool = published
            .iter()
            .find(|tool| tool.name == definition.name)
            .unwrap();
        let mut expected = definition.input_schema;
        expected.as_object_mut().unwrap().remove("$schema");
        expected.as_object_mut().unwrap().remove("title");
        expected["properties"]["capability"] =
            json!({"type":"string","minLength":46,"maxLength":46});
        expected["required"]
            .as_array_mut()
            .unwrap()
            .push(json!("capability"));
        assert_eq!(Value::Object((*tool.input_schema).clone()), expected);
    }
    let create = published
        .iter()
        .find(|tool| tool.name == mini_apps::CREATE_TOOL)
        .unwrap();
    let branches=create.input_schema["properties"]["createSurface"]["properties"]["components"]["items"]["oneOf"].as_array().unwrap();
    assert_eq!(branches.len(), 5);
    let fixture: Value = serde_json::from_str(mini_apps::FIXTURE).unwrap();
    for (id, component) in [
        ("root", "TaskDocument"),
        ("selector", "TaskListSelector"),
        ("tasks", "TaskCollection"),
        ("search", "DocumentSearch"),
        ("document", "DocumentHost"),
    ] {
        let branch = branches
            .iter()
            .find(|branch| branch["properties"]["id"]["const"] == id)
            .unwrap();
        assert_eq!(branch["properties"]["component"]["const"], component);
        assert_eq!(branch["additionalProperties"], false);
        for required in ["id", "component"] {
            assert!(
                branch["required"]
                    .as_array()
                    .unwrap()
                    .contains(&json!(required))
            );
        }
        let mut wrong = fixture.clone();
        wrong["createSurface"]["components"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|c| c["id"] == id)
            .unwrap()["id"] = "wrong-id".into();
        assert!(Declaration::from_value(&wrong).is_err());
    }
    let tasks = branches
        .iter()
        .find(|branch| branch["properties"]["id"]["const"] == "tasks")
        .unwrap();
    assert_eq!(
        tasks["oneOf"],
        json!([{"required":["bucket"]},{"required":["list"]}])
    );
    assert!(Declaration::from_value(&fixture).is_ok());
    let mut list = fixture.clone();
    let tasks = list["createSurface"]["components"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|c| c["id"] == "tasks")
        .unwrap();
    tasks.as_object_mut().unwrap().remove("bucket");
    tasks["list"] = json!({"workspace":"current","listId":uuid::Uuid::new_v4()});
    assert!(Declaration::from_value(&list).is_ok());
    let tasks = list["createSurface"]["components"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|c| c["id"] == "tasks")
        .unwrap();
    tasks["bucket"] = "today".into();
    assert!(Declaration::from_value(&list).is_err());
    let tasks = list["createSurface"]["components"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|c| c["id"] == "tasks")
        .unwrap();
    tasks.as_object_mut().unwrap().remove("bucket");
    tasks.as_object_mut().unwrap().remove("list");
    assert!(Declaration::from_value(&list).is_err());
    let create_bytes = serde_json::to_vec(create).unwrap().len();
    println!(
        "mini-app create tool: {create_bytes} bytes; shared page limit {}",
        crate::tool_pages::PAGE_BYTES
    );
    assert!(
        create_bytes < crate::tool_pages::PAGE_BYTES,
        "normative creation schema must fit a shared MCP page"
    );
}
struct Fake(std::sync::atomic::AtomicUsize);
impl Backend for Fake {
    fn doctor(&self) -> Result<Value, ToolError> {
        panic!("no doctor I/O in fixture")
    }
    fn invoke(&self, _: &RequestContext, name: &str, arguments: Value) -> Result<Value, ToolError> {
        assert_eq!(name, "mini_app_tool");
        let request: koru_ai_wire::MiniAppToolRequest = serde_json::from_value(arguments).unwrap();
        assert!(request.capability.valid());
        assert!(request.arguments.get("capability").is_none());
        self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(json!({"status":"offered","application_acknowledged":false}))
    }
}
#[test]
fn mini_app_mcp_wrappers_require_capability_and_bound_arguments_before_backend() {
    let fake = Arc::new(Fake(std::sync::atomic::AtomicUsize::new(0)));
    let server = KoruMcp::new(fake.clone(), RequestContext::local());
    let mut envelope: Value = serde_json::from_str(koru_ai_core::mini_apps::FIXTURE).unwrap();
    assert_eq!(
        server
            .mini_app_call(koru_ai_core::mini_apps::CREATE_TOOL, envelope.clone())
            .is_error,
        Some(true)
    );
    envelope["capability"] = koru_ai_core::mini_apps::Capability::mint().0.into();
    let mut too_large = envelope.clone();
    too_large["unknown"] = "x".repeat(4097).into();
    assert_eq!(
        server
            .mini_app_call(koru_ai_core::mini_apps::CREATE_TOOL, too_large)
            .is_error,
        Some(true)
    );
    let disabled = server
        .clone()
        .with_toolsets(Toolsets::parse("docs").unwrap());
    assert_eq!(
        disabled
            .mini_app_call(koru_ai_core::mini_apps::CREATE_TOOL, envelope.clone())
            .is_error,
        Some(true)
    );
    assert_eq!(fake.0.load(std::sync::atomic::Ordering::Relaxed), 0);
    assert_ne!(
        server
            .mini_app_call(koru_ai_core::mini_apps::CREATE_TOOL, envelope)
            .is_error,
        Some(true)
    );
    assert_eq!(fake.0.load(std::sync::atomic::Ordering::Relaxed), 1);
    for tool in tools() {
        assert!(tool.input_schema["properties"].get("capability").is_some());
        assert!(
            tool.input_schema["required"]
                .as_array()
                .unwrap()
                .contains(&json!("capability"))
        );
    }
}
