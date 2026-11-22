// SPDX-License-Identifier: MPL-2.0
//! MCP presents the existing bounded native vocabulary; the AI worker owns
//! exact-attempt authority and delivery. This bridge owns no host state.
use super::*;
pub(crate) const NAMES: &[&str] = &[
    koru_ai_core::mini_apps::CREATE_TOOL,
    koru_ai_core::mini_apps::LIST_TOOL,
    koru_ai_core::mini_apps::UPDATE_TOOL,
];
pub(crate) fn tools() -> Vec<rmcp::model::Tool> {
    koru_ai_core::mini_apps::definitions().into_iter().map(|definition| {
        let mut schema=definition.input_schema;
        schema["properties"]["capability"]=json!({"type":"string","minLength":46,"maxLength":46});
        schema["required"].as_array_mut().expect("owned closed object").push(json!("capability"));
        // Hand-authored oneOf branches are normative: generic tagged-union
        // flattening discards id/component pairing and bucket/list exclusion.
        let schema=schema.as_object_mut().unwrap();
        schema.remove("$schema");
        schema.remove("title");
        rmcp::model::Tool::new(definition.name, format!("{} Requires the expiring capability supplied by the current Koru chat message; unavailable to an unbound external client. Never echo that capability in prose.",definition.description), Arc::new(schema.clone()))
            .with_title(definition.title).with_annotations(rmcp::model::ToolAnnotations::new().read_only(true).open_world(false))
    }).collect()
}
pub(crate) fn request(
    name: &str,
    mut arguments: Value,
) -> Result<koru_ai_wire::MiniAppToolRequest, ToolError> {
    if !NAMES.contains(&name) {
        return Err(ToolError::invalid("unknown mini-app tool"));
    }
    let capability=arguments.as_object_mut().and_then(|object|object.remove("capability")).and_then(|v|v.as_str().map(str::to_owned)).map(koru_ai_core::mini_apps::Capability).filter(|c|c.valid()).ok_or_else(||ToolError::invalid("This tool needs a live Koru chat capability. Send a new request from the native host."))?;
    koru_ai_core::mini_apps::bounded_json(&arguments)
        .map_err(|_| ToolError::invalid("mini-app arguments exceed the bounded catalog"))?;
    Ok(koru_ai_wire::MiniAppToolRequest {
        capability,
        name: name.into(),
        call_id: uuid::Uuid::new_v4().to_string(),
        arguments,
    })
}
#[cfg(test)]
mod tests;
