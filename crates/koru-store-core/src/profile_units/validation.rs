// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::ids::{ActionId, ReminderId};
use crate::personal_attributes::{EntityRef, PersonalAttributeValue, validate_attribute_key};
use crate::reminders::{LocalTime, OccurrenceAction, ReminderRule, ToneId, WeekStartPreference};

pub(super) fn validate(doc: &LoroDoc, unit: ProfileUnitRef) -> Result<Compatibility> {
    let meta = doc.get_map("meta");
    let schema = integer(&meta, "schema_version")
        .filter(|v| *v > 0)
        .ok_or_else(|| corrupt("unit format is absent or invalid"))?;
    let (key, expected) = match unit {
        ProfileUnitRef::Personal { owner, workspace } => {
            if string(&meta, "workspace_id") != Some(workspace.to_storage()) {
                return Err(scope_mismatch());
            }
            ("owner_profile_id", owner)
        }
        _ => ("profile_id", unit.profile()),
    };
    if string(&meta, key) != Some(expected.to_storage()) {
        return Err(scope_mismatch());
    }
    let compatibility = Compatibility::against(Some(schema), unit.schema());
    // Preserve future formats without pretending to understand their new roots.
    // Their metadata still has to name this exact unit; every write is gated.
    if !compatibility.writable() {
        return Ok(compatibility);
    }
    let (roots, keys): (&[&str], &[&str]) = match unit {
        ProfileUnitRef::Identity { .. } => (
            &["meta"],
            &[
                "schema_version",
                "profile_id",
                "name",
                "purpose",
                "avatar",
                "icon",
                "color",
                "account",
                "locale",
                "created_at",
                "kind",
            ],
        ),
        ProfileUnitRef::Personal { .. } => (
            &["meta", "personal_attributes", "reminders"],
            &["schema_version", "owner_profile_id", "workspace_id"],
        ),
        ProfileUnitRef::Private { .. } => (
            &["meta", "reminders", "personal_dictionary"],
            &["schema_version", "profile_id"],
        ),
        ProfileUnitRef::LegacyPrivate { .. } => return Err(scope_mismatch()),
    };
    let LoroValue::Map(values) = doc.get_deep_value() else {
        return Err(corrupt("unit root is not a map"));
    };
    for (key, value) in values.iter() {
        if !roots.contains(&key.as_str()) || !matches!(value, LoroValue::Map(_)) {
            return Err(scope_mismatch());
        }
    }
    allowed_keys(&meta, keys)?;
    match unit {
        ProfileUnitRef::Identity { .. } => {
            for key in ["purpose", "avatar", "icon", "color", "account"] {
                if meta.get(key).is_some() && string(&meta, key).is_none() {
                    return Err(corrupt("invalid optional identity field"));
                }
            }
            super::identity::read_profile(doc)?;
        }
        ProfileUnitRef::Personal { workspace, .. } => validate_personal(doc, workspace)?,
        ProfileUnitRef::Private { .. } => validate_private(doc)?,
        _ => unreachable!(),
    }
    Ok(compatibility)
}

/// Personal v2 (`docs/features/root-catalog/TECHNICAL.final.md` §5.1): the
/// header names the Workspace and the format, and nothing in it names an
/// owner. `owner_profile_id` may remain from a migrated v1 history as a
/// string or a redaction null; it is ignored for ownership and never
/// trusted for authorization. The content validator is the v1 one, unchanged.
pub(super) fn validate_personal_v2(doc: &LoroDoc, workspace: WorkspaceId) -> Result<Compatibility> {
    let meta = doc.get_map("meta");
    let schema = integer(&meta, "schema_version")
        .filter(|v| *v > 0)
        .ok_or_else(|| corrupt("unit format is absent or invalid"))?;
    if string(&meta, "workspace_id") != Some(workspace.to_storage()) {
        return Err(scope_mismatch());
    }
    // A v1 document is scoped to a Profile pair. It reaches this address
    // through the migration, which stamps the version; relabeling is refused.
    if schema < PERSONAL_V2_CRDT_SCHEMA_VERSION {
        return Err(scope_mismatch());
    }
    let compatibility = Compatibility::against(Some(schema), PERSONAL_V2_CRDT_SCHEMA_VERSION);
    if !compatibility.writable() {
        return Ok(compatibility);
    }
    let LoroValue::Map(values) = doc.get_deep_value() else {
        return Err(corrupt("unit root is not a map"));
    };
    for (key, value) in values.iter() {
        if !["meta", "personal_attributes", "reminders"].contains(&key.as_str())
            || !matches!(value, LoroValue::Map(_))
        {
            return Err(scope_mismatch());
        }
    }
    allowed_keys(
        &meta,
        &["schema_version", "workspace_id", "owner_profile_id"],
    )?;
    if let Some(owner) = meta.get("owner_profile_id")
        && !matches!(
            owner,
            ValueOrContainer::Value(LoroValue::String(_) | LoroValue::Null)
        )
    {
        return Err(corrupt("invalid legacy owner field"));
    }
    validate_personal(doc, workspace)?;
    Ok(compatibility)
}

fn allowed_keys(map: &loro::LoroMap, allowed: &[&str]) -> Result<()> {
    if map.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(scope_mismatch());
    }
    Ok(())
}
fn child(value: ValueOrContainer) -> Result<loro::LoroMap> {
    match value {
        ValueOrContainer::Container(value) => value
            .into_map()
            .map_err(|_| corrupt("unit child is not a map")),
        _ => Err(corrupt("unit child is not a mergeable map")),
    }
}
fn atomic(value: ValueOrContainer) -> Result<LoroValue> {
    match value {
        ValueOrContainer::Value(value) => Ok(value),
        _ => Err(corrupt("unit value is not atomic")),
    }
}
fn target(entity: &EntityRef, workspace: WorkspaceId) -> Result<()> {
    if entity.workspace() == workspace {
        Ok(())
    } else {
        Err(scope_mismatch())
    }
}
fn reminder_id(key: &str) -> Result<ReminderId> {
    ReminderId::parse(key)
        .filter(|id| id.to_storage() == key)
        .ok_or_else(|| corrupt("invalid reminder id"))
}
fn action_id(key: &str) -> Result<ActionId> {
    ActionId::parse(key)
        .filter(|id| id.to_storage() == key)
        .ok_or_else(|| corrupt("invalid action id"))
}
fn validate_personal(doc: &LoroDoc, workspace: WorkspaceId) -> Result<()> {
    for (key, value) in entries(&doc.get_map("personal_attributes")) {
        let entity = EntityRef::parse(&key)
            .filter(|entity| entity.to_storage() == key)
            .ok_or_else(|| corrupt("invalid personal entity"))?;
        target(&entity, workspace)?;
        for (key, value) in entries(&child(value)?) {
            validate_attribute_key(&key)?;
            let value = PersonalAttributeValue::from_loro(&atomic(value)?)
                .ok_or_else(|| corrupt("invalid personal attribute"))?;
            value.validate()?;
        }
    }
    let reminders = doc.get_map("reminders");
    allowed_keys(
        &reminders,
        &["rules", "actions", "deleted", "cleared_actions"],
    )?;
    for (collection, value) in entries(&reminders) {
        for (key, value) in entries(&child(value)?) {
            let id = reminder_id(&key)?;
            match collection.as_str() {
                "rules" => {
                    let rule = ReminderRule::from_loro(&atomic(value)?)
                        .ok_or_else(|| corrupt("invalid reminder rule"))?;
                    if rule.id != id {
                        return Err(corrupt("reminder key differs from rule id"));
                    }
                    rule.validate()?;
                    target(&rule.target, workspace)?;
                }
                "deleted" => {
                    if !matches!(atomic(value)?, LoroValue::Bool(true)) {
                        return Err(corrupt("invalid reminder delete fact"));
                    }
                }
                "actions" | "cleared_actions" => {
                    for (key, value) in entries(&child(value)?) {
                        let id = action_id(&key)?;
                        let value = atomic(value)?;
                        if collection == "cleared_actions" {
                            if !matches!(value, LoroValue::Bool(true)) {
                                return Err(corrupt("invalid reminder clear fact"));
                            }
                        } else {
                            let action = OccurrenceAction::from_loro(&value)
                                .ok_or_else(|| corrupt("invalid reminder action"))?;
                            if action.id != id {
                                return Err(corrupt("action key differs from action id"));
                            }
                        }
                    }
                }
                _ => unreachable!(),
            }
        }
    }
    Ok(())
}
fn validate_private(doc: &LoroDoc) -> Result<()> {
    let reminders = doc.get_map("reminders");
    allowed_keys(&reminders, &["settings"])?;
    if let Some(value) = reminders.get("settings") {
        let settings = child(value)?;
        allowed_keys(&settings, &["end_of_day", "default_tone", "week_start"])?;
        for (key, value) in entries(&settings) {
            let LoroValue::String(value) = atomic(value)? else {
                return Err(corrupt("invalid reminder setting"));
            };
            let value = value.to_string();
            let valid = match key.as_str() {
                "end_of_day" => LocalTime::parse(&value).is_some_and(|v| v.to_storage() == value),
                "default_tone" => {
                    serde_json::from_value::<ToneId>(serde_json::Value::String(value)).is_ok()
                }
                "week_start" => {
                    serde_json::from_value::<WeekStartPreference>(serde_json::Value::String(value))
                        .is_ok()
                }
                _ => false,
            };
            if !valid {
                return Err(corrupt("invalid reminder setting"));
            }
        }
    }
    for (language, value) in entries(&doc.get_map("personal_dictionary")) {
        super::content::validate_language_tag(&language)?;
        for (word, value) in entries(&child(value)?) {
            super::content::validate_dictionary_word(&word)?;
            if !matches!(atomic(value)?, LoroValue::Bool(true)) {
                return Err(corrupt("invalid dictionary entry"));
            }
        }
    }
    Ok(())
}
