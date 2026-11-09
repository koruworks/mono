// SPDX-License-Identifier: MPL-2.0

//! The shape check every catalog candidate passes before it is published:
//! loaded from a row, staged from a peer, or exported to a row.

use loro::{LoroDoc, LoroValue};

use super::lifecycle::{
    DeletionRecord, Facts, Placement, PurgeRecord, RestoreRecord, atomic, deletion_key, restore_key,
};
use super::{
    CatalogUnitKind, CatalogUnitRef, CatalogUnitRefusal as Refusal, K_CURRENT, K_PROFILE_ID,
    K_SCHEMA_VERSION, K_UNIT_KIND, K_WORKSPACE_ID, R_DELETIONS, R_META, R_PLACEMENT, R_PURGED,
    R_RESTORES,
};
use crate::unit_doc::{entries, integer, string};
use crate::version::{CATALOG_UNIT_FORMAT_VERSION, Compatibility};

/// Validate a complete document against its address. The header is checked
/// before the format version, so a too-new document must still name this
/// exact unit to be held at all; its unknown roots are then left alone.
pub(super) fn validate(doc: &LoroDoc, unit: CatalogUnitRef) -> Result<Compatibility, Refusal> {
    let meta = doc.get_map(R_META);
    let schema = integer(&meta, K_SCHEMA_VERSION)
        .filter(|v| *v > 0)
        .ok_or(Refusal::SchemaAbsent)?;
    let kind = string(&meta, K_UNIT_KIND);
    if kind.as_deref() != Some(unit.kind().as_str()) {
        return Err(Refusal::WrongUnitKind { found: kind });
    }
    let (subject_key, other_key) = match unit.kind() {
        CatalogUnitKind::Profile => (K_PROFILE_ID, K_WORKSPACE_ID),
        CatalogUnitKind::Workspace => (K_WORKSPACE_ID, K_PROFILE_ID),
    };
    if string(&meta, subject_key) != Some(unit.subject_id()) {
        return Err(Refusal::HeaderMismatch { key: subject_key });
    }
    let compatibility = Compatibility::against(Some(schema), CATALOG_UNIT_FORMAT_VERSION);
    // Preserve future formats without pretending to understand their roots.
    // Their metadata still has to name this exact unit; every write is gated.
    if !compatibility.writable() {
        return Ok(compatibility);
    }

    let roots: &[&str] = match unit.kind() {
        CatalogUnitKind::Profile => &[R_META, R_DELETIONS, R_RESTORES, R_PURGED],
        CatalogUnitKind::Workspace => &[R_META, R_PLACEMENT, R_DELETIONS, R_RESTORES, R_PURGED],
    };
    let LoroValue::Map(values) = doc.get_deep_value() else {
        return Err(Refusal::RootNotMap {
            root: String::new(),
        });
    };
    for (root, value) in values.iter() {
        if !roots.contains(&root.as_str()) {
            return Err(Refusal::UnknownRoot { root: root.clone() });
        }
        if !matches!(value, LoroValue::Map(_)) {
            return Err(Refusal::RootNotMap { root: root.clone() });
        }
    }
    allowed_keys(doc, R_META, &[K_SCHEMA_VERSION, K_UNIT_KIND, subject_key])?;
    debug_assert!(
        meta.get(other_key).is_none(),
        "allowed_keys admits only the subject key"
    );

    let mut placement = None;
    if unit.kind() == CatalogUnitKind::Workspace {
        allowed_keys(doc, R_PLACEMENT, &[K_CURRENT])?;
        if let Some(value) = doc.get_map(R_PLACEMENT).get(K_CURRENT) {
            let path = format!("{R_PLACEMENT}.{K_CURRENT}");
            placement = Some(Placement::from_loro(&atomic(value, &path)?, &path)?);
        }
    }

    for (key, value) in entries(&doc.get_map(R_DELETIONS)) {
        let path = format!("{R_DELETIONS}.{key}");
        deletion_key(&key).ok_or_else(|| Refusal::MalformedUuid { path: path.clone() })?;
        DeletionRecord::from_loro(&atomic(value, &path)?, &path)?;
    }
    for (key, value) in entries(&doc.get_map(R_RESTORES)) {
        let path = format!("{R_RESTORES}.{key}");
        let id = restore_key(&key).ok_or_else(|| Refusal::MalformedUuid { path: path.clone() })?;
        RestoreRecord::from_loro(&atomic(value, &path)?, id, &path)?;
    }
    for (key, value) in entries(&doc.get_map(R_PURGED)) {
        let path = format!("{R_PURGED}.{key}");
        deletion_key(&key).ok_or_else(|| Refusal::MalformedUuid { path: path.clone() })?;
        PurgeRecord::from_loro(&atomic(value, &path)?, &path)?;
    }

    // An absent placement is pending on a unit that has not been committed.
    // Once a live Workspace unit is published it must say where it is, even
    // if the answer is "explicitly nowhere".
    if unit.kind() == CatalogUnitKind::Workspace && placement.is_none() && (Facts { doc }).is_live()
    {
        return Err(Refusal::PlacementRequired);
    }
    Ok(compatibility)
}

fn allowed_keys(doc: &LoroDoc, root: &'static str, allowed: &[&str]) -> Result<(), Refusal> {
    if let Some(key) = doc
        .get_map(root)
        .keys()
        .find(|key| !allowed.contains(&key.as_str()))
    {
        return Err(Refusal::UnknownKey {
            root,
            key: key.to_string(),
        });
    }
    Ok(())
}
