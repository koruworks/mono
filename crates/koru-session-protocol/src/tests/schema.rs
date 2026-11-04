// SPDX-License-Identifier: MPL-2.0

use std::path::Path;

use crate::{Attachment, schema};

#[test]
fn every_registered_wire_type_has_a_generated_schema() {
    let catalogue = schema::catalogue();
    assert_eq!(catalogue.len(), schema::TYPE_NAMES.len());
    for name in schema::TYPE_NAMES {
        assert!(catalogue.contains_key(name), "missing schema for {name}");
    }
}

#[test]
fn every_public_model_declaration_is_registered() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = manifest.join("src");
    let mut declared = std::collections::BTreeSet::new();
    for entry in std::fs::read_dir(&source).unwrap().flatten() {
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("rs")
            || matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some("lib.rs")
            )
        {
            continue;
        }
        let text = std::fs::read_to_string(path).unwrap();
        for line in text.lines().map(str::trim) {
            for marker in ["pub struct ", "pub enum "] {
                if let Some(rest) = line.strip_prefix(marker) {
                    let name = rest
                        .split(|character: char| {
                            character == '<'
                                || character == '('
                                || character == '{'
                                || character.is_whitespace()
                        })
                        .next()
                        .unwrap();
                    if !name.starts_with('$') {
                        declared.insert(name.to_owned());
                    }
                }
            }
        }
        for invocation in ["uuid_id!(", "token_type!(", "positive_counter!("] {
            let mut remainder = text.as_str();
            while let Some(index) = remainder.find(invocation) {
                remainder = &remainder[index + invocation.len()..];
                let name = remainder
                    .lines()
                    .map(str::trim)
                    .find(|line| {
                        !line.is_empty()
                            && !line.starts_with("///")
                            && !line.starts_with("//")
                            && !line.starts_with("#")
                    })
                    .and_then(|line| line.trim_end_matches(',').split_whitespace().next())
                    .unwrap();
                // The same guard the declaration scan applies: a `$name` is a
                // macro's forwarding arm, not a public type.
                if !name.starts_with('$') {
                    declared.insert(name.to_owned());
                }
            }
        }
    }

    // Parse errors are local construction errors, not serialized protocol data.
    declared.remove("IdParseError");
    declared.remove("TokenParseError");
    declared.remove("ApprovalChoiceError");
    declared.remove("QuestionAnswerError");
    let registered: std::collections::BTreeSet<_> = schema::TYPE_NAMES
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    assert_eq!(
        declared, registered,
        "public protocol type missing from schema catalogue"
    );
}

#[test]
fn generated_schema_matches_the_checked_in_snapshot() {
    let value = serde_json::to_value(schema::catalogue()).unwrap();
    let text = format!("{}\n", serde_json::to_string_pretty(&value).unwrap());
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../reference/schemas/koru-session-protocol.schema.json");
    if std::env::var_os("KORU_UPDATE_SESSION_SCHEMA").is_some() {
        std::fs::write(&path, &text).unwrap();
    }
    let checked_in: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        checked_in, value,
        "run KORU_UPDATE_SESSION_SCHEMA=1 cargo test -p koru-session-protocol generated_schema_matches_the_checked_in_snapshot"
    );
}

#[test]
fn attachment_schema_exposes_metadata_not_storage_paths_or_bytes() {
    let text = serde_json::to_string(&schemars::schema_for!(Attachment)).unwrap();
    for forbidden in ["storage_path", "root_path", "file_name", "payload", "bytes"] {
        assert!(
            !text.contains(forbidden),
            "{forbidden} leaked into Attachment schema: {text}"
        );
    }
}
