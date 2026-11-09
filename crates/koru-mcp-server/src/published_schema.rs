// SPDX-License-Identifier: MPL-2.0

//! What `tools/list` publishes of each tool's input: names, types, enums,
//! `required` and closed objects — and no prose.
//!
//! The request structs are derived with schemars, which writes every Rust doc
//! comment into the schema as a `description`, repeats a tagged enum as one
//! complete object per variant, and hangs `$schema`, `title`, `$defs` and
//! Rust integer widths on top. The reply that came out of that was one JSON
//! line of 57,071 bytes, which every connected agent pays for in its context
//! and which Grok CLI 1.0.34 failed to decode in two of seven sessions: its
//! reader lost an 8,192-byte chunk of the line and then registered no Koru
//! tool at all. Compacted, the reply is about 17 KB; the floor with every
//! parameter typed is above 8 KiB, so this makes the list cheaper for every
//! agent and does not by itself make Grok reliable (ADR-433-compacttools).
//!
//! So the router is rewritten once, after it is built, by one rule set:
//!
//! - **Prose lives in the tool's description, not in its schema.** Every
//!   `description`, `title` and `$schema` is dropped. A doc comment on a
//!   request field is a note for the next developer; what an agent needs from
//!   it is written once, condensed, in the `#[tool]` description, and the
//!   full contract is `koru://docs/model` (`get_documentation`).
//! - **A tagged union becomes one closed object.** A `oneOf` whose branches
//!   all carry one `const` property (`op`, `type`) becomes an object whose tag
//!   is an `enum`, whose properties are the union of the branches', and whose
//!   `description` is generated from the branches — `insert(node_id,
//!   parent_id, kind, text, after_node_id?)` — so it cannot drift from the
//!   Rust enum. Which fields an op takes is still enforced where it always
//!   was: serde's `deny_unknown_fields` on the server.
//! - **Nothing refers elsewhere.** A local `$ref` is replaced by its
//!   definition and `$defs` is dropped, so a bridge that strips `$defs`
//!   cannot leave a dangling reference.
//! - **Only what constrains a value stays.** A numeric `format` names a Rust
//!   width (`int64`, `uint`) rather than anything a caller chooses, and a
//!   `default` of `null`, `[]` or `false` says no more than *optional* does.
//! - **Each `type` is one name.** After compaction, type arrays become
//!   equivalent `anyOf` branches for clients that cannot map a type array.
//!   Null, required fields and sibling constraints retain their meaning.
//!
//! What is kept: every property name, every type, every enum value, every
//! `required`, `additionalProperties: false`, `minimum`/`maximum`, and a
//! default that names a value (`"open"`, `"relevance"`).

use rmcp::{handler::server::router::tool::ToolRouter, model::JsonObject};
use serde_json::{Map, Value};
use std::sync::Arc;

/// Rewrite every tool's published input schema in place.
pub(crate) fn compact_router<S>(router: &mut ToolRouter<S>) {
    for route in router.map.values_mut() {
        route.attr.input_schema = Arc::new(compact_input_schema(&route.attr.input_schema));
    }
}

/// One tool's input schema, compacted by the rules above.
pub(crate) fn compact_input_schema(schema: &JsonObject) -> JsonObject {
    let defs = schema
        .get("$defs")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut root = schema.clone();
    root.remove("$defs");
    let mut schema = schemars::Schema::from(compact(root, &defs));
    portable_types(&mut schema);
    schema.as_object().expect("an object schema").clone()
}

/// Run after compaction: tagged-union merging can itself produce type arrays.
/// Walk schema positions only, so a property named `type` or JSON data inside
/// a default/enum is never mistaken for a schema keyword.
fn portable_types(schema: &mut schemars::Schema) {
    schemars::transform::transform_subschemas(&mut portable_types, schema);
    let Some(object) = schema.as_object_mut() else {
        return;
    };
    let Some(Value::Array(types)) = object.get("type") else {
        return;
    };
    let branches = Value::Array(
        types
            .iter()
            .map(|name| serde_json::json!({ "type": name }))
            .collect(),
    );
    object.remove("type");
    if object.contains_key("anyOf") {
        // Both constraints must hold. Never overwrite an existing alternative
        // set, or distribute siblings such as enum/required only to one branch.
        let existing = std::mem::take(object);
        object.insert(
            "allOf".into(),
            serde_json::json!([
                existing, { "anyOf": branches }
            ]),
        );
    } else {
        object.insert("anyOf".into(), branches);
    }
}

/// Keywords whose value is itself one schema.
const SCHEMA_KEYWORDS: &[&str] = &["items", "additionalProperties", "not"];
/// Keywords whose value is a list of schemas.
const SCHEMA_LIST_KEYWORDS: &[&str] = &["anyOf", "oneOf", "allOf", "prefixItems"];

fn compact(mut schema: JsonObject, defs: &JsonObject) -> JsonObject {
    if let Some(Value::String(reference)) = schema.remove("$ref") {
        match reference
            .strip_prefix("#/$defs/")
            .and_then(|name| defs.get(name))
            .and_then(Value::as_object)
        {
            // Sibling keywords (a `default`) win over the definition's own.
            Some(definition) => {
                let mut merged = definition.clone();
                merged.extend(schema);
                return compact(merged, defs);
            }
            None => {
                schema.insert("$ref".into(), Value::String(reference));
            }
        }
    }

    for keyword in ["$schema", "title", "description"] {
        schema.remove(keyword);
    }
    if matches!(
        schema.get("type").and_then(Value::as_str),
        Some("integer" | "number")
    ) || type_names(&schema)
        .iter()
        .any(|t| t == "integer" || t == "number")
    {
        schema.remove("format");
    }
    if matches!(
        schema.get("default"),
        Some(Value::Null | Value::Bool(false))
    ) || schema
        .get("default")
        .is_some_and(|d| d.as_array().is_some_and(Vec::is_empty))
    {
        schema.remove("default");
    }

    if let Some(Value::Object(properties)) = schema.remove("properties") {
        let properties = properties
            .into_iter()
            .map(|(name, member)| (name, compact_value(member, defs)))
            .collect();
        schema.insert("properties".into(), Value::Object(properties));
    }
    for keyword in SCHEMA_KEYWORDS {
        if let Some(value) = schema.remove(*keyword) {
            schema.insert((*keyword).into(), compact_value(value, defs));
        }
    }
    for keyword in SCHEMA_LIST_KEYWORDS {
        if let Some(Value::Array(branches)) = schema.remove(*keyword) {
            let branches = branches
                .into_iter()
                .map(|branch| compact_value(branch, defs))
                .collect();
            schema.insert((*keyword).into(), Value::Array(branches));
        }
    }

    let schema = collapse_nullable(schema);
    let schema = collapse_const_enum(schema);
    flatten_tagged_union(schema)
}

fn compact_value(value: Value, defs: &JsonObject) -> Value {
    match value {
        Value::Object(schema) => Value::Object(compact(schema, defs)),
        other => other,
    }
}

fn type_names(schema: &JsonObject) -> Vec<String> {
    match schema.get("type") {
        Some(Value::String(one)) => vec![one.clone()],
        Some(Value::Array(many)) => many
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

/// `anyOf: [X, {"type":"null"}]` — how schemars spells an optional composite —
/// becomes X, with `null` added to its `type` when it has one.
fn collapse_nullable(mut schema: JsonObject) -> JsonObject {
    let Some(Value::Array(branches)) = schema.get("anyOf") else {
        return schema;
    };
    let null = Value::Object(Map::from_iter([("type".into(), Value::from("null"))]));
    if branches.len() != 2 || !branches.contains(&null) {
        return schema;
    }
    let Some(Value::Object(inner)) = branches.iter().find(|b| **b != null).cloned() else {
        return schema;
    };
    schema.remove("anyOf");
    let mut merged = inner;
    let mut types = type_names(&merged);
    if !types.is_empty() && !types.iter().any(|t| t == "null") {
        types.push("null".into());
        merged.insert(
            "type".into(),
            Value::Array(types.into_iter().map(Value::from).collect()),
        );
    }
    merged.extend(schema);
    merged
}

/// `oneOf` of bare `const` branches — a documented unit enum — becomes an
/// `enum`.
fn collapse_const_enum(mut schema: JsonObject) -> JsonObject {
    let Some(Value::Array(branches)) = schema.get("oneOf") else {
        return schema;
    };
    let values: Option<Vec<Value>> = branches
        .iter()
        .map(|branch| {
            let branch = branch.as_object()?;
            let only_const = branch.keys().all(|key| key == "const" || key == "type");
            only_const.then(|| branch.get("const").cloned()).flatten()
        })
        .collect();
    let Some(values) = values else {
        return schema;
    };
    schema.remove("oneOf");
    if values.iter().all(Value::is_string) {
        schema.insert("type".into(), Value::from("string"));
    }
    schema.insert("enum".into(), Value::Array(values));
    schema
}

/// The one property every branch pins with `const`, if there is one.
fn union_tag(branches: &[JsonObject]) -> Option<String> {
    let first = branches.first()?.get("properties")?.as_object()?;
    first
        .iter()
        .filter(|(_, member)| member.get("const").is_some())
        .map(|(name, _)| name.clone())
        .find(|name| {
            branches.iter().all(|branch| {
                branch
                    .get("properties")
                    .and_then(|p| p.get(name))
                    .and_then(|member| member.get("const"))
                    .is_some()
            })
        })
}

fn flatten_tagged_union(mut schema: JsonObject) -> JsonObject {
    let Some(Value::Array(branches)) = schema.get("oneOf") else {
        return schema;
    };
    let Some(branches) = branches
        .iter()
        .map(|branch| branch.as_object().cloned())
        .collect::<Option<Vec<_>>>()
    else {
        return schema;
    };
    let Some(tag) = union_tag(&branches) else {
        return schema;
    };
    schema.remove("oneOf");

    let mut tags = Vec::new();
    let mut properties = Map::new();
    let mut summary = Vec::new();
    let closed = branches
        .iter()
        .all(|branch| branch.get("additionalProperties") == Some(&Value::Bool(false)));
    for branch in &branches {
        let members = branch
            .get("properties")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let required: Vec<&str> = branch
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let name = members[&tag]["const"].clone();
        let mut fields: Vec<String> = required
            .iter()
            .filter(|field| **field != tag)
            .map(|field| (*field).to_owned())
            .collect();
        fields.extend(
            members
                .keys()
                .filter(|field| **field != tag && !required.contains(&field.as_str()))
                .map(|field| format!("{field}?")),
        );
        summary.push(format!(
            "{}({})",
            name.as_str().unwrap_or_default(),
            fields.join(", ")
        ));
        tags.push(name);
        for (field, member) in members {
            if field == tag {
                continue;
            }
            let merged = match properties.remove(&field) {
                None => member,
                Some(existing) => merge(existing, member),
            };
            properties.insert(field, merged);
        }
    }
    let mut tag_schema = Map::new();
    if tags.iter().all(Value::is_string) {
        tag_schema.insert("type".into(), Value::from("string"));
    }
    tag_schema.insert("enum".into(), Value::Array(tags));
    properties.insert(tag.clone(), Value::Object(tag_schema));

    schema.insert("type".into(), Value::from("object"));
    schema.insert("properties".into(), Value::Object(properties));
    schema.insert("required".into(), Value::Array(vec![Value::from(tag)]));
    if closed {
        schema.insert("additionalProperties".into(), Value::Bool(false));
    }
    schema.insert("description".into(), Value::from(summary.join(" | ")));
    schema
}

/// Two branches' schemas for one field name. Equal ones are one schema; two
/// that differ only in `type` accept either (`value` is a string, a number or
/// a boolean by variant); anything else accepts any value and leaves the
/// check to the server, which is where it happens anyway.
fn merge(existing: Value, incoming: Value) -> Value {
    if existing == incoming {
        return existing;
    }
    let (Some(a), Some(b)) = (existing.as_object(), incoming.as_object()) else {
        return Value::Object(Map::new());
    };
    let without_type = |schema: &JsonObject| {
        let mut rest = schema.clone();
        rest.remove("type");
        rest
    };
    if without_type(a) != without_type(b) {
        return Value::Object(Map::new());
    }
    let mut types = type_names(a);
    for name in type_names(b) {
        if !types.contains(&name) {
            types.push(name);
        }
    }
    let mut merged = a.clone();
    merged.insert(
        "type".into(),
        Value::Array(types.into_iter().map(Value::from).collect()),
    );
    Value::Object(merged)
}

#[cfg(test)]
mod tests;
