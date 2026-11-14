// SPDX-License-Identifier: MPL-2.0

//! The approved schema, kept independent of the private planning checkout.
//! Inline its local references so the same definitions work both in the
//! standalone catalogue and under OpenAPI's components path.
use super::{CloudPlanResponseV1, CloudUsageWorkspacesResponseV1};
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde_json::Value;
use std::borrow::Cow;

pub(super) fn expand(value: &Value, definitions: &Value) -> Value {
    match value {
        Value::Object(fields) => {
            if let Some(reference) = fields.get("$ref").and_then(Value::as_str) {
                let name = reference
                    .strip_prefix("#/$defs/")
                    .expect("local schema reference");
                return expand(&definitions[name], definitions);
            }
            Value::Object(
                fields
                    .iter()
                    .map(|(k, v)| (k.clone(), expand(v, definitions)))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(|v| expand(v, definitions)).collect()),
        _ => value.clone(),
    }
}

fn approved(name: &str) -> Schema {
    let source: Value = serde_json::from_str(include_str!("../cloud_plan.schema.json"))
        .expect("approved schema JSON");
    expand(&source["$defs"][name], &source["$defs"])
        .try_into()
        .expect("approved schema object")
}

macro_rules! schema {
    ($type:ident, $definition:literal) => {
        impl JsonSchema for $type {
            fn schema_name() -> Cow<'static, str> {
                stringify!($type).into()
            }
            fn json_schema(_: &mut SchemaGenerator) -> Schema {
                approved($definition)
            }
        }
    };
}
schema!(CloudPlanResponseV1, "plan");
schema!(CloudUsageWorkspacesResponseV1, "page");
