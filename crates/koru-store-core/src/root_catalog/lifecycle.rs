// SPDX-License-Identifier: MPL-2.0

//! The records a catalog unit stores and the pure reducers over them.
//!
//! Every record is one atomic `LoroValue::Map` under a UUID key, written
//! whole and never edited: `deletions`, `restores` and `purged` grow, and
//! `placement.current` is replaced as a single register. Reducers read the
//! current state and nothing else — no clocks, no receiver, no side channel —
//! so two replicas holding the same state answer the same thing.

use std::collections::{BTreeMap, BTreeSet};

use loro::{LoroDoc, LoroValue, ValueOrContainer};
use uuid::Uuid;

use super::{
    CatalogUnitRefusal as Refusal, F_AT_MS, F_BATCH_ID, F_MOVE_ID, F_OBSERVED_DELETIONS,
    F_PROFILE_ID, F_RETENTION_MS, K_CURRENT, R_DELETIONS, R_PLACEMENT, R_PURGED, R_RESTORES,
    canonical_id, canonical_uuid,
};
use crate::ids::{DeletionId, ProfileId, RestoreId};
use crate::service::TRASH_RETENTION_MS;
use crate::unit_doc::entries;

/// This user's current placement of a Workspace. `profile: None` is
/// *explicitly unassigned* — a value somebody wrote, not an absence.
/// `move_id` is the opaque id of the move operation that wrote it; two
/// replicas that agree on the register agree on this id too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    pub profile: Option<ProfileId>,
    pub move_id: Uuid,
}

/// One deletion fact. `batch_id` names the Profile-deletion operation that
/// cascaded here, or is `None` for a deletion of this subject alone. It is an
/// opaque operation UUID: never a Profile or Workspace id, and never a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeletionRecord {
    pub at_ms: i64,
    pub retention_ms: i64,
    pub batch_id: Option<Uuid>,
}

impl DeletionRecord {
    /// A deletion with the product's one retention default,
    /// [`TRASH_RETENTION_MS`] (30 days). There is no second constant.
    pub fn new(at_ms: i64, batch_id: Option<Uuid>) -> Self {
        Self {
            at_ms,
            retention_ms: TRASH_RETENTION_MS,
            batch_id,
        }
    }
}

/// One restore fact. It clears exactly the deletions it names; a deletion it
/// did not observe stays active. An empty list is meaningful only as the
/// receipt of a batch action that found nothing to clear on this subject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreRecord {
    pub at_ms: i64,
    pub observed_deletions: Vec<DeletionId>,
    pub batch_id: Option<Uuid>,
}

/// Terminal evidence that a deletion's content was purged. Kept after the
/// content rows are gone so a stale replica cannot resurrect the subject;
/// never makes anything live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PurgeRecord {
    pub at_ms: i64,
}

// ------------------------------------------------------------- encoding

fn map(pairs: Vec<(&str, LoroValue)>) -> LoroValue {
    let map: std::collections::HashMap<String, LoroValue> = pairs
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect();
    LoroValue::from(map)
}

fn optional_uuid(value: Option<Uuid>) -> LoroValue {
    match value {
        Some(id) => LoroValue::from(id.to_string()),
        None => LoroValue::Null,
    }
}

impl Placement {
    pub(super) fn to_loro(self) -> LoroValue {
        map(vec![
            (
                F_PROFILE_ID,
                optional_uuid(self.profile.map(|profile| profile.as_uuid())),
            ),
            (F_MOVE_ID, LoroValue::from(self.move_id.to_string())),
        ])
    }

    pub(super) fn from_loro(value: &LoroValue, path: &str) -> Result<Self, Refusal> {
        let record = Record::new(value, path, &[F_PROFILE_ID, F_MOVE_ID])?;
        let profile = match record.optional_string(F_PROFILE_ID)? {
            None => None,
            Some(text) => Some(
                canonical_id(&text, ProfileId::parse, |id| id.to_storage()).ok_or_else(|| {
                    Refusal::MalformedUuid {
                        path: record.path(F_PROFILE_ID),
                    }
                })?,
            ),
        };
        Ok(Self {
            profile,
            move_id: record.uuid(F_MOVE_ID)?,
        })
    }
}

impl DeletionRecord {
    fn validate(&self, path: &str) -> Result<(), Refusal> {
        nonnegative(self.at_ms, path, F_AT_MS)?;
        nonnegative(self.retention_ms, path, F_RETENTION_MS)
    }

    pub(super) fn to_loro(self) -> LoroValue {
        map(vec![
            (F_AT_MS, LoroValue::from(self.at_ms)),
            (F_RETENTION_MS, LoroValue::from(self.retention_ms)),
            (F_BATCH_ID, optional_uuid(self.batch_id)),
        ])
    }

    pub(super) fn from_loro(value: &LoroValue, path: &str) -> Result<Self, Refusal> {
        let record = Record::new(value, path, &[F_AT_MS, F_RETENTION_MS, F_BATCH_ID])?;
        let record = Self {
            at_ms: record.integer(F_AT_MS)?,
            retention_ms: record.integer(F_RETENTION_MS)?,
            batch_id: record.optional_uuid(F_BATCH_ID)?,
        };
        record.validate(path)?;
        Ok(record)
    }
}

impl RestoreRecord {
    fn validate(&self, id: RestoreId, path: &str) -> Result<(), Refusal> {
        nonnegative(self.at_ms, path, F_AT_MS)?;
        if self.observed_deletions.is_empty() && self.batch_id.is_none() {
            return Err(Refusal::RestoreWithoutObservations { restore: id });
        }
        Ok(())
    }

    pub(super) fn to_loro(&self) -> LoroValue {
        map(vec![
            (F_AT_MS, LoroValue::from(self.at_ms)),
            (
                F_OBSERVED_DELETIONS,
                LoroValue::from(
                    self.observed_deletions
                        .iter()
                        .map(|id| LoroValue::from(id.to_storage()))
                        .collect::<Vec<_>>(),
                ),
            ),
            (F_BATCH_ID, optional_uuid(self.batch_id)),
        ])
    }

    pub(super) fn from_loro(value: &LoroValue, id: RestoreId, path: &str) -> Result<Self, Refusal> {
        let record = Record::new(value, path, &[F_AT_MS, F_OBSERVED_DELETIONS, F_BATCH_ID])?;
        let Some(LoroValue::List(observed)) = record.map.get(F_OBSERVED_DELETIONS) else {
            return Err(Refusal::RecordShape {
                path: record.path(F_OBSERVED_DELETIONS),
            });
        };
        let observed_deletions = observed
            .iter()
            .map(|item| match item {
                LoroValue::String(text) => canonical_id(text, DeletionId::parse, |id| {
                    id.to_storage()
                })
                .ok_or_else(|| Refusal::MalformedUuid {
                    path: record.path(F_OBSERVED_DELETIONS),
                }),
                _ => Err(Refusal::RecordShape {
                    path: record.path(F_OBSERVED_DELETIONS),
                }),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let record = Self {
            at_ms: record.integer(F_AT_MS)?,
            observed_deletions,
            batch_id: record.optional_uuid(F_BATCH_ID)?,
        };
        record.validate(id, path)?;
        Ok(record)
    }
}

impl PurgeRecord {
    fn validate(&self, path: &str) -> Result<(), Refusal> {
        nonnegative(self.at_ms, path, F_AT_MS)
    }

    pub(super) fn to_loro(self) -> LoroValue {
        map(vec![(F_AT_MS, LoroValue::from(self.at_ms))])
    }

    pub(super) fn from_loro(value: &LoroValue, path: &str) -> Result<Self, Refusal> {
        let record = Record::new(value, path, &[F_AT_MS])?;
        let record = Self {
            at_ms: record.integer(F_AT_MS)?,
        };
        record.validate(path)?;
        Ok(record)
    }
}

fn nonnegative(value: i64, path: &str, field: &str) -> Result<(), Refusal> {
    if value < 0 {
        return Err(Refusal::NegativeMilliseconds {
            path: format!("{path}.{field}"),
        });
    }
    Ok(())
}

/// One atomic record map with exactly the expected fields.
struct Record<'a> {
    map: &'a loro::LoroMapValue,
    path: &'a str,
}

impl<'a> Record<'a> {
    fn new(value: &'a LoroValue, path: &'a str, fields: &[&str]) -> Result<Self, Refusal> {
        let LoroValue::Map(map) = value else {
            return Err(Refusal::RecordShape {
                path: path.to_string(),
            });
        };
        if map.len() != fields.len() || !fields.iter().all(|field| map.contains_key(*field)) {
            return Err(Refusal::RecordShape {
                path: path.to_string(),
            });
        }
        Ok(Self { map, path })
    }

    fn path(&self, field: &str) -> String {
        format!("{}.{field}", self.path)
    }

    fn integer(&self, field: &str) -> Result<i64, Refusal> {
        match self.map.get(field) {
            Some(LoroValue::I64(value)) => Ok(*value),
            _ => Err(Refusal::RecordShape {
                path: self.path(field),
            }),
        }
    }

    fn optional_string(&self, field: &str) -> Result<Option<String>, Refusal> {
        match self.map.get(field) {
            Some(LoroValue::Null) => Ok(None),
            Some(LoroValue::String(text)) => Ok(Some(text.to_string())),
            _ => Err(Refusal::RecordShape {
                path: self.path(field),
            }),
        }
    }

    fn uuid(&self, field: &str) -> Result<Uuid, Refusal> {
        match self.map.get(field) {
            Some(LoroValue::String(text)) => {
                canonical_uuid(text).ok_or_else(|| Refusal::MalformedUuid {
                    path: self.path(field),
                })
            }
            _ => Err(Refusal::RecordShape {
                path: self.path(field),
            }),
        }
    }

    fn optional_uuid(&self, field: &str) -> Result<Option<Uuid>, Refusal> {
        self.optional_string(field)?
            .map(|text| {
                canonical_uuid(&text).ok_or_else(|| Refusal::MalformedUuid {
                    path: self.path(field),
                })
            })
            .transpose()
    }
}

/// The atomic value under a key, or the refusal for a child container.
pub(super) fn atomic(value: ValueOrContainer, path: &str) -> Result<LoroValue, Refusal> {
    match value {
        ValueOrContainer::Value(value) => Ok(value),
        ValueOrContainer::Container(_) => Err(Refusal::NotAtomic {
            path: path.to_string(),
        }),
    }
}

// ------------------------------------------------- writers and reducers

/// The lifecycle and placement state of one unit, borrowed from its document.
/// Reads tolerate nothing: a document reaches here only after validation, so
/// a record that fails to decode is skipped as the unreachable case it is.
pub(super) struct Facts<'a> {
    pub(super) doc: &'a LoroDoc,
}

impl Facts<'_> {
    fn records<K: Ord + Copy, V>(
        &self,
        root: &str,
        key: impl Fn(&str) -> Option<K>,
        decode: impl Fn(K, &LoroValue, &str) -> Result<V, Refusal>,
    ) -> BTreeMap<K, V> {
        let mut out = BTreeMap::new();
        for (name, value) in entries(&self.doc.get_map(root)) {
            let path = format!("{root}.{name}");
            let Some(id) = key(&name) else { continue };
            let Ok(value) = atomic(value, &path) else {
                continue;
            };
            if let Ok(record) = decode(id, &value, &path) {
                out.insert(id, record);
            }
        }
        out
    }

    pub fn deletions(&self) -> BTreeMap<DeletionId, DeletionRecord> {
        self.records(R_DELETIONS, deletion_key, |_, value, path| {
            DeletionRecord::from_loro(value, path)
        })
    }

    pub fn restores(&self) -> BTreeMap<RestoreId, RestoreRecord> {
        self.records(R_RESTORES, restore_key, |id, value, path| {
            RestoreRecord::from_loro(value, id, path)
        })
    }

    pub fn purged(&self) -> BTreeMap<DeletionId, PurgeRecord> {
        self.records(R_PURGED, deletion_key, |_, value, path| {
            PurgeRecord::from_loro(value, path)
        })
    }

    /// Deletion ids no restore has named. An unseen concurrent delete stays
    /// here; a restore with an empty observation list removes nothing.
    pub fn active_deletions(&self) -> BTreeSet<DeletionId> {
        let observed: BTreeSet<DeletionId> = self
            .restores()
            .into_values()
            .flat_map(|restore| restore.observed_deletions)
            .collect();
        self.deletions()
            .into_keys()
            .filter(|id| !observed.contains(id))
            .collect()
    }

    pub fn is_deleted(&self) -> bool {
        !self.active_deletions().is_empty()
    }

    /// Neither deleted nor purged. The state a committed Workspace unit must
    /// hold a placement in.
    pub fn is_live(&self) -> bool {
        !self.is_deleted() && self.purged().is_empty()
    }

    pub fn placement(&self) -> Option<Placement> {
        let path = format!("{R_PLACEMENT}.{K_CURRENT}");
        let value = atomic(self.doc.get_map(R_PLACEMENT).get(K_CURRENT)?, &path).ok()?;
        Placement::from_loro(&value, &path).ok()
    }

    /// Replace the register with one whole value in one commit.
    pub fn set_placement(&self, placement: &Placement) -> Result<(), Refusal> {
        self.doc
            .get_map(R_PLACEMENT)
            .insert(K_CURRENT, placement.to_loro())
            .map_err(|_| Refusal::Crdt)?;
        self.doc.commit();
        Ok(())
    }

    pub fn record_deletion(&self, id: DeletionId, record: &DeletionRecord) -> Result<(), Refusal> {
        record.validate(&format!("{R_DELETIONS}.{id}"))?;
        self.append(R_DELETIONS, id.as_uuid(), record.to_loro())
    }

    pub fn record_restore(&self, id: RestoreId, record: &RestoreRecord) -> Result<(), Refusal> {
        record.validate(id, &format!("{R_RESTORES}.{id}"))?;
        self.append(R_RESTORES, id.as_uuid(), record.to_loro())
    }

    pub fn record_purge(&self, id: DeletionId, record: &PurgeRecord) -> Result<(), Refusal> {
        record.validate(&format!("{R_PURGED}.{id}"))?;
        self.append(R_PURGED, id.as_uuid(), record.to_loro())
    }

    /// Append one immutable fact. An existing key is refused rather than
    /// overwritten: a fact is written once, and a retry that finds it present
    /// has already succeeded.
    fn append(&self, root: &str, id: Uuid, value: LoroValue) -> Result<(), Refusal> {
        let map = self.doc.get_map(root);
        let key = id.to_string();
        if map.get(&key).is_some() {
            return Err(Refusal::DuplicateLifecycleId { id });
        }
        map.insert(&key, value).map_err(|_| Refusal::Crdt)?;
        self.doc.commit();
        Ok(())
    }
}

pub(super) fn deletion_key(key: &str) -> Option<DeletionId> {
    canonical_id(key, DeletionId::parse, |id| id.to_storage())
}

pub(super) fn restore_key(key: &str) -> Option<RestoreId> {
    canonical_id(key, RestoreId::parse, |id| id.to_storage())
}
