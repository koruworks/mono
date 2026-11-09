// SPDX-License-Identifier: MPL-2.0

use super::*;
use serde_json::json;

fn compacted(schema: Value) -> Value {
    Value::Object(compact_input_schema(schema.as_object().unwrap()))
}

#[test]
fn a_tagged_union_becomes_one_closed_object_that_names_every_variant() {
    let schema = json!({
        "type": "object",
        "properties": {
            "operations": {
                "type": "array",
                "items": {
                    "oneOf": [
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "description": "Remove one node.",
                            "properties": {
                                "op": {"type": "string", "const": "remove"},
                                "node_id": {"type": "string", "description": "UUID."}
                            },
                            "required": ["op", "node_id"]
                        },
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "op": {"type": "string", "const": "set_value"},
                                "node_id": {"type": "string"},
                                "value": {"type": "number", "format": "double"},
                                "note": {"type": ["string", "null"], "default": null}
                            },
                            "required": ["op", "node_id", "value"]
                        },
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "op": {"type": "string", "const": "set_flag"},
                                "value": {"type": "boolean"}
                            },
                            "required": ["op", "value"]
                        }
                    ]
                }
            }
        }
    });
    assert_eq!(
        compacted(schema),
        json!({
            "type": "object",
            "properties": {
                "operations": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["op"],
                        "description": "remove(node_id) | set_value(node_id, value, note?) | set_flag(value)",
                        "properties": {
                            "op": {"type": "string", "enum": ["remove", "set_value", "set_flag"]},
                            "node_id": {"type": "string"},
                            "note": {"anyOf": [{"type": "string"}, {"type": "null"}]},
                            "value": {"anyOf": [{"type": "number"}, {"type": "boolean"}]}
                        }
                    }
                }
            }
        })
    );
}

#[test]
fn a_reference_is_inlined_with_its_default_and_defs_are_gone() {
    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "Request",
        "description": "A developer's note.",
        "type": "object",
        "$defs": {
            "Sort": {"type": "string", "enum": ["due", "priority"], "description": "Order."},
            "Window": {
                "type": "object",
                "properties": {"after_ms": {"type": ["integer", "null"], "format": "int64"}}
            }
        },
        "properties": {
            "sort": {"$ref": "#/$defs/Sort", "default": "due"},
            "window": {"anyOf": [{"$ref": "#/$defs/Window"}, {"type": "null"}], "default": null},
            "limit": {"type": ["integer", "null"], "format": "uint", "minimum": 0},
            "tags": {"type": "array", "items": {"type": "string"}, "default": []},
            "title": {"type": "string", "description": "A property named title stays."}
        }
    });
    assert_eq!(
        compacted(schema),
        json!({
            "type": "object",
            "properties": {
                "sort": {"type": "string", "enum": ["due", "priority"], "default": "due"},
                "window": {
                    "anyOf": [{"type": "object"}, {"type": "null"}],
                    "properties": {"after_ms": {"anyOf": [{"type": "integer"}, {"type": "null"}]}}
                },
                "limit": {"anyOf": [{"type": "integer"}, {"type": "null"}], "minimum": 0},
                "tags": {"type": "array", "items": {"type": "string"}},
                "title": {"type": "string"}
            }
        })
    );
}

#[test]
fn a_documented_unit_enum_becomes_an_enum() {
    let schema = json!({
        "type": "object",
        "properties": {
            "lifecycle": {"oneOf": [
                {"type": "string", "const": "open", "description": "Not done."},
                {"type": "string", "const": "all"}
            ]}
        }
    });
    assert_eq!(
        compacted(schema),
        json!({
            "type": "object",
            "properties": {"lifecycle": {"type": "string", "enum": ["open", "all"]}}
        })
    );
}

#[test]
fn fields_that_differ_in_more_than_type_accept_anything_and_leave_it_to_the_server() {
    let schema = json!({
        "oneOf": [
            {"type": "object", "properties": {"t": {"const": "a"}, "x": {"type": "string", "enum": ["p"]}}, "required": ["t"]},
            {"type": "object", "properties": {"t": {"const": "b"}, "x": {"type": "object"}}, "required": ["t"]}
        ]
    });
    let out = compacted(schema);
    assert_eq!(out["properties"]["x"], json!({}));
    assert!(
        out.get("additionalProperties").is_none(),
        "branches were open"
    );
}

#[test]
fn published_local_and_hosted_catalogs_never_use_type_arrays() {
    fn check(schema: &mut schemars::Schema) {
        assert!(
            !schema.get("type").is_some_and(Value::is_array),
            "{schema:?}"
        );
        schemars::transform::transform_subschemas(&mut check, schema);
    }
    for tool in crate::workspace_tool_catalog()
        .into_iter()
        .chain(crate::HostCapabilities::rung1().catalog())
    {
        let mut schema = schemars::Schema::try_from(tool.input_schema).unwrap();
        check(&mut schema);
    }
}

#[test]
fn nullable_required_fields_keep_their_constraints_and_default_data() {
    let data = json!({"type": ["string", "null"]});
    assert_eq!(
        compacted(json!({
            "type": "object", "required": ["page_size"], "additionalProperties": false,
            "properties": {
                "page_size": {"type": ["integer", "null"], "minimum": 1, "maximum": 100},
                "type": {"type": ["string", "null"], "enum": ["open", null]},
                "payload": {"default": data, "enum": [data]}
            }
        })),
        json!({
            "type": "object", "required": ["page_size"], "additionalProperties": false,
            "properties": {
                "page_size": {"anyOf": [{"type": "integer"}, {"type": "null"}], "minimum": 1, "maximum": 100},
                "type": {"anyOf": [{"type": "string"}, {"type": "null"}], "enum": ["open", null]},
                "payload": {"default": data, "enum": [data]}
            }
        })
    );
}

#[test]
fn a_type_array_does_not_overwrite_existing_alternatives_or_conjunctions() {
    let input = json!({
        "type": ["integer", "null"],
        "anyOf": [{"const": 3}, {"const": null}],
        "allOf": [{"minimum": 0}]
    });
    assert_eq!(
        compacted(input),
        json!({"allOf": [
            {"anyOf": [{"const": 3}, {"const": null}], "allOf": [{"minimum": 0}]},
            {"anyOf": [{"type": "integer"}, {"type": "null"}]}
        ]})
    );
}
