// SPDX-License-Identifier: MPL-2.0

//! The registry is closed, and these tests are what closes it.
//!
//! The SP-6 plan document is the checklist. Every count and every membership
//! assertion below is a sentence from it, so a row added without a decision
//! fails here rather than appearing silently in an API.

use super::*;
use crate::sections::{AiPatch, GlobalPatch, GlobalSettings, TerminalPatch, TerminalSettings};

/// The field names a serialized value of `T` carries.
fn fields_of<T: serde::Serialize>(value: &T) -> Vec<String> {
    let serde_json::Value::Object(object) = serde_json::to_value(value).unwrap() else {
        panic!("a section serializes as an object");
    };
    object.keys().cloned().collect()
}

#[test]
fn every_registry_row_is_a_field_of_its_section() {
    let global = fields_of(&GlobalSettings::default());
    let terminal = fields_of(&TerminalSettings::default());
    for spec in REGISTRY {
        match spec.section {
            Section::Global => assert!(
                global.contains(&spec.field.to_owned()),
                "`{}` is in the registry but not in GlobalSettings",
                spec.field
            ),
            Section::Terminal => assert!(
                terminal.contains(&spec.field.to_owned()),
                "`{}` is in the registry but not in TerminalSettings",
                spec.field
            ),
            // The AI section is a facade over another authority, so its rows
            // name *patch* fields rather than struct fields — the settings
            // shape is nested (`openai.model`) while the patch is flat
            // (`openai_model`), because a patch names one thing at a time.
            Section::Ai => {}
        }
    }
}

#[test]
fn every_section_field_is_a_registry_row() {
    for (section, fields) in [
        (Section::Global, fields_of(&GlobalSettings::default())),
        (Section::Terminal, fields_of(&TerminalSettings::default())),
    ] {
        let rows: Vec<_> = settings_in(section).map(|spec| spec.field).collect();
        for field in fields {
            assert!(
                rows.contains(&field.as_str()),
                "`{field}` is in the {} section but has no registry row — a \
                 setting exists because there is a row for it",
                section.token()
            );
        }
    }
}

/// The field names a patch type declares.
///
/// Read from the schema rather than from a serialized value: a patch skips its
/// absent fields, so an empty one serializes to `{}` and would prove nothing.
fn patch_fields(schema: schemars::Schema) -> Vec<String> {
    schema
        .as_value()
        .get("properties")
        .and_then(|value| value.as_object())
        .map(|object| object.keys().cloned().collect())
        .unwrap_or_default()
}

#[test]
fn every_registry_row_is_patchable_except_the_read_only_one() {
    // The registry is the set of things somebody can change, with exactly one
    // documented exception: whether an OpenAI key is in the vault is a fact
    // about the vault, reported and never set from here.
    let global = patch_fields(schemars::schema_for!(GlobalPatch));
    let terminal = patch_fields(schemars::schema_for!(TerminalPatch));
    let ai = patch_fields(schemars::schema_for!(AiPatch));
    assert!(!global.is_empty() && !terminal.is_empty() && !ai.is_empty());

    for spec in REGISTRY {
        if spec.field == "openai_credential_configured" {
            assert!(
                !ai.contains(&spec.field.to_owned()),
                "the credential flag is read-only: a secret never travels \
                 through this authority in either direction"
            );
            continue;
        }
        let patch = match spec.section {
            Section::Global => &global,
            Section::Terminal => &terminal,
            Section::Ai => &ai,
        };
        assert!(
            patch.contains(&spec.field.to_owned()),
            "`{}` is in the registry but no patch can change it",
            spec.field
        );
    }
}

#[test]
fn the_plan_frozen_registry_is_exactly_this_size() {
    // From SP-6's "closed registry" block. A row added or removed without a
    // decision changes one of these numbers.
    let count = |section, storage| {
        settings_in(section)
            .filter(|spec| spec.storage == storage)
            .count()
    };
    assert_eq!(
        count(Section::Global, StorageClass::Roaming),
        25,
        "Global roaming: theme, icon pack, three caret values, four fonts, the \
         document text size (DOCFONTSIZE, 2026-10-30), the base color per \
         theme (2026-10-31), density, \
         zoom, view mode, canvas background, pinned title, popout close, \
         skin tone, four dictation values, and two spelling values \
         (spell-check plan P5, D7)"
    );
    assert_eq!(
        count(Section::Global, StorageClass::DeviceLocal),
        14,
        "Global device-local: close behavior, global shortcut, start on \
         login, its reminder explanation record, automatic update checks, \
         global dictation enable + chord, where a delegated run's own folder \
         is made (ADR-365), the session recorder's prefill (SESSIONRECORD), \
         the coding agents turned off (Agent providers, 2026-11-02), the \
         two default permissions for new chats (D39), the default agent \
         (D40) and the agents' order (D41)"
    );
    assert_eq!(
        count(Section::Terminal, StorageClass::Roaming),
        8,
        "Terminal roams: theme, font, zoom, four cursor values, composer \
         default"
    );
    assert_eq!(
        settings_in(Section::Terminal)
            .filter(|spec| spec.storage != StorageClass::Roaming)
            .count(),
        0,
        "the whole Terminal section roams — that is the decision"
    );
    assert_eq!(
        settings_in(Section::Ai)
            .filter(|spec| spec.storage == StorageClass::Delegated)
            .count(),
        7,
        "every AI row is delegated: the authority faces `ai.db` and the vault, \
         it does not own them"
    );
    assert_eq!(REGISTRY.len(), 54);
}

#[test]
fn the_two_axes_are_independent() {
    // The whole point of having two: if every roaming setting were Global and
    // every device-local one were not, one axis would do.
    assert!(
        settings_in(Section::Global).any(|spec| spec.storage == StorageClass::Roaming)
            && settings_in(Section::Global).any(|spec| spec.storage == StorageClass::DeviceLocal),
        "one section spans both storage classes"
    );
    assert!(
        REGISTRY
            .iter()
            .filter(|spec| spec.storage == StorageClass::Roaming)
            .map(|spec| spec.section)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            > 1,
        "one storage class spans more than one section"
    );
}

#[test]
fn a_setting_lands_in_the_file_its_storage_class_implies() {
    for spec in REGISTRY {
        let expected = match (spec.section, spec.storage) {
            (Section::Global, StorageClass::Roaming) => Backing::PreferencesConf,
            (Section::Global, StorageClass::DeviceLocal) => Backing::DesktopJson,
            (Section::Terminal, StorageClass::Roaming) => Backing::TerminalConf,
            (Section::Ai, StorageClass::Delegated) => Backing::AiAuthority,
            other => panic!("`{}` has an undeclared pairing {other:?}", spec.field),
        };
        assert_eq!(spec.backing, expected, "`{}`", spec.field);
    }
}

#[test]
fn the_font_roles_are_the_four_that_can_follow_the_app() {
    let fonts: Vec<_> = REGISTRY
        .iter()
        .filter(|spec| spec.kind == ValueKind::Font && spec.section == Section::Global)
        .map(|spec| spec.field)
        .collect();
    assert_eq!(
        fonts,
        ["ui_font", "body_font", "headings_font", "code_font"],
        "exactly four Global font roles model follow-the-app explicitly"
    );
    // The terminal's font is a fifth font role and deliberately not one of
    // these: it is its own value, mono-restricted, in its own section.
    assert!(
        settings_in(Section::Terminal).any(|spec| spec.kind == ValueKind::Font),
        "the terminal has its own font, not Global's code_font"
    );
}

#[test]
fn every_exemption_says_what_it_is_and_why_it_is_not_a_setting() {
    assert!(
        EXEMPTIONS.len() >= 7,
        "the exemptions are the answer to 'why is this not a setting?', and \
         an unanswered one is how a closed registry becomes a property bag"
    );
    for (what, why) in EXEMPTIONS {
        assert!(!what.is_empty());
        assert!(
            why.len() > 40,
            "`{what}` is excluded without a reason anybody could check"
        );
    }
    assert!(
        EXEMPTIONS.iter().any(|(what, _)| what.contains("renderer")),
        "the renderer passthrough is stated"
    );
    assert!(
        EXEMPTIONS.iter().any(|(what, _)| what.contains("secrets")),
        "AI secrets keep their existing authority"
    );
}

#[test]
fn every_row_documents_itself() {
    for spec in REGISTRY {
        assert!(
            spec.doc.len() > 20,
            "`{}` has no usable documentation",
            spec.field
        );
        assert!(!spec.field.is_empty());
    }
}

#[test]
fn the_section_tokens_round_trip() {
    for section in Section::ALL {
        assert_eq!(Section::parse(section.token()), Some(*section));
    }
    assert_eq!(Section::parse("windows"), None);
    assert_eq!(Section::parse("Global"), None, "tokens are lowercase");
}
