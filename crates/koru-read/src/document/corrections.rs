// SPDX-License-Identifier: MPL-2.0

//! User-authored revisions are separate from replaceable OCR generations.
//! Revisions are immutable; their parent links retain concurrent/offline edits.
//! Reset is a revision too, so an old peer cannot resurrect a reset correction.
use super::*;
use std::collections::BTreeSet;

const CORRECTIONS: &str = "corrections/v1";

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Corrections(pub BTreeMap<String, CorrectionRevision>);

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CorrectionRevision {
    pub parents: BTreeSet<String>,
    /// None is an explicit Reset to OCR; Some(empty) is a corrected blank.
    pub text: Option<PageText>,
}

impl Corrections {
    pub fn merge_from(&mut self, other: &Self) {
        for (id, revision) in &other.0 {
            self.0.entry(id.clone()).or_insert_with(|| revision.clone());
        }
    }
    pub fn heads(&self) -> Vec<String> {
        let parents: BTreeSet<_> = self.0.values().flat_map(|r| r.parents.iter()).collect();
        self.0
            .keys()
            .filter(|id| !parents.contains(id))
            .cloned()
            .collect()
    }

    /// Concurrent heads remain available for review. Ordering only chooses
    /// which one is displayed/indexed until a person resolves those heads.
    pub fn selected(&self) -> Option<(&str, &CorrectionRevision)> {
        let id = self.heads().pop()?;
        self.0
            .get_key_value(&id)
            .map(|(id, value)| (id.as_str(), value))
    }

    pub fn text(&self) -> Option<&PageText> {
        self.selected()?.1.text.as_ref()
    }
}

impl ReadingDoc {
    pub fn merge_corrections(&self, hash: &str, history: &Corrections) -> Result<()> {
        merge(&self.ensure_entry(hash)?, history)?;
        self.doc.commit();
        Ok(())
    }
    pub fn corrections(&self, hash: &str) -> Corrections {
        self.entry(hash).as_ref().map(read).unwrap_or_default()
    }

    /// Compare current heads before adding a revision. A stale editor must
    /// retain its draft and refresh; it cannot silently replace newer work.
    pub fn correct(
        &self,
        hash: &str,
        revision: &str,
        expected_heads: &[String],
        text: Option<PageText>,
    ) -> Result<bool> {
        if revision.is_empty() || revision.len() > 128 {
            return Err(Error::Inconsistent);
        }
        let current = self.corrections(hash);
        let mut expected = expected_heads.to_vec();
        expected.sort();
        expected.dedup();
        if current.heads() != expected || current.0.contains_key(revision) {
            return Ok(false);
        }
        let entry = self.ensure_entry(hash)?;
        merge(
            &entry,
            &Corrections(BTreeMap::from([(
                revision.to_owned(),
                CorrectionRevision {
                    parents: expected.into_iter().collect(),
                    text,
                },
            )])),
        )?;
        self.doc.commit();
        Ok(true)
    }
}

pub(super) fn read(entry: &LoroMap) -> Corrections {
    let Some(versions) = child_map(entry, CORRECTIONS) else {
        return Corrections::default();
    };
    let mut result = BTreeMap::new();
    for id in versions.keys() {
        let Some(version) = child_map(&versions, &id) else {
            continue;
        };
        let text = get_string(&version, "text").map(PageText::new);
        let reset = version
            .get("reset")
            .is_some_and(|value| matches!(value, ValueOrContainer::Value(LoroValue::Bool(true))));
        if text.is_none() && !reset {
            continue;
        }
        let parents = child_map(&version, "parents")
            .map(|map| map.keys().map(|key| key.to_string()).collect())
            .unwrap_or_default();
        result.insert(id.to_string(), CorrectionRevision { parents, text });
    }
    Corrections(result)
}

pub(super) fn merge(entry: &LoroMap, corrections: &Corrections) -> Result<()> {
    if corrections.0.is_empty() {
        return Ok(());
    }
    let current = read(entry);
    let versions = entry.ensure_mergeable_map(CORRECTIONS)?;
    for (id, revision) in &corrections.0 {
        if let Some(existing) = current.0.get(id) {
            if existing != revision {
                return Err(Error::Inconsistent);
            }
            continue;
        }
        let version = versions.ensure_mergeable_map(id)?;
        if let Some(text) = &revision.text {
            version.insert("text", text.as_str())?;
        }
        version.insert("reset", revision.text.is_none())?;
        let parents = version.ensure_mergeable_map("parents")?;
        for parent in &revision.parents {
            parents.insert(parent, true)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
