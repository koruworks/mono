// SPDX-License-Identifier: MPL-2.0

//! The one validated staging boundary every independently addressed Loro unit
//! shares: Profile identity/private/personal units and the root-catalog units.
//!
//! A unit is its own `LoroDoc` with its own version vector, addressed by a
//! value outside the document (a Profile id, a Workspace id, a user scope plus
//! a Workspace id). The mechanism here is address-agnostic: it knows how to
//! author a fresh lineage, load durable bytes, gate writes on
//! [`Compatibility`], and **stage** an import — fork the held document, apply
//! frames to the fork, validate the complete candidate, and only then hand the
//! fork back so a caller can adopt it. What a valid document looks like for a
//! given address is the [`UnitAddress`] implementation's business.
//!
//! **`Ok` with pending ancestors is not applied state.** A Loro import whose
//! causal ancestors are missing returns `Ok` and applies nothing; the
//! operations wait inside the document for ancestors that may never come. The
//! fork is disposable, so a pending import is reported as [`Staged::Pending`]
//! and the held document is untouched — never `Ok`, never a partial merge.

use std::fmt;

use loro::{
    ContainerID, ExportMode, JsonOpContent, LoroDoc, LoroValue, ValueOrContainer, VersionVector,
};

use crate::errors::{Code, Error};
use crate::version::Compatibility;

/// What the mechanism itself can refuse, before any address-specific rule.
/// Each address family maps these into its own refusal type so a caller sees
/// one vocabulary per unit kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnitFailure {
    /// Durable bytes did not decode as a Loro snapshot.
    UndecodableSnapshot,
    /// Durable bytes decoded but their history has gaps; a stored snapshot is
    /// always complete, so this is corruption rather than a transport state.
    SnapshotMissingAncestors,
    /// An incoming frame did not decode.
    UndecodableFrame,
    /// An incoming frame needs ancestors the held unit does not have. Only
    /// produced where the caller's contract has no [`Staged::Pending`] value.
    FrameMissingAncestors,
    /// The held unit was written by a newer build and is open read-only.
    ReadOnly { found: i64, understood: i64 },
    /// An incoming frame would move the unit to a format this build cannot
    /// write; it is held back rather than merged into a writable unit.
    IncomingTooNew { found: i64, understood: i64 },
    /// A Loro operation failed. Not a validation outcome.
    Crdt,
}

impl From<UnitFailure> for Error {
    fn from(failure: UnitFailure) -> Self {
        match failure {
            UnitFailure::UndecodableSnapshot => {
                Error::new(Code::CorruptState, "invalid unit snapshot")
            }
            UnitFailure::SnapshotMissingAncestors => Error::new(
                Code::CorruptState,
                "unit snapshot has missing causal ancestors",
            ),
            UnitFailure::UndecodableFrame => Error::new(Code::CrdtRejected, "unit frame rejected"),
            UnitFailure::FrameMissingAncestors => Error::new(
                Code::CrdtRejected,
                "unit frame has missing causal ancestors",
            ),
            UnitFailure::ReadOnly { .. } => Error::new(
                Code::SchemaTooNew,
                "Profile unit format is newer than this build",
            ),
            UnitFailure::IncomingTooNew { .. } => Error::new(
                Code::SchemaTooNew,
                "incoming unit format is newer than this build",
            ),
            UnitFailure::Crdt => Error::new(Code::StorageFailed, "unit CRDT operation failed"),
        }
    }
}

/// The address a unit is validated against, and the rules that go with it.
pub(crate) trait UnitAddress: Copy + fmt::Debug {
    /// The refusal vocabulary this address family reports. The mechanism's
    /// own failures are converted into it.
    type Refusal: From<UnitFailure>;

    /// Validate a complete document against this address: header, scope,
    /// allowed roots and content. Returns the compatibility of the format it
    /// found; a too-new document is still a valid *read-only* unit.
    fn validate(&self, doc: &LoroDoc) -> Result<Compatibility, Self::Refusal>;

    /// A report hook after one frame merged into the staged fork. Never
    /// refuses; identity units use it to log a rival lineage.
    fn merged(&self, _before: &LoroDoc, _after: &LoroDoc) {}
}

/// The outcome of staging frames against a held unit.
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Staged<T> {
    /// Every frame applied; this is the complete validated candidate.
    Applied(T),
    /// At least one frame needs ancestors the unit does not have. Nothing was
    /// applied and the held unit is unchanged. The sender needs the unit's
    /// current [`VersionVector`] to send the gap.
    Pending,
}

impl<T> Staged<T> {
    pub fn is_pending(&self) -> bool {
        matches!(self, Self::Pending)
    }

    pub fn applied(self) -> Option<T> {
        match self {
            Self::Applied(value) => Some(value),
            Self::Pending => None,
        }
    }

    pub(crate) fn map<U>(self, f: impl FnOnce(T) -> U) -> Staged<U> {
        match self {
            Self::Applied(value) => Staged::Applied(f(value)),
            Self::Pending => Staged::Pending,
        }
    }
}

pub(crate) struct UnitDoc<A: UnitAddress> {
    pub(crate) doc: LoroDoc,
    pub(crate) unit: A,
    pub(crate) compatibility: Compatibility,
}

impl<A: UnitAddress> UnitDoc<A> {
    /// Author a fresh lineage. `init` writes the header (and may set the
    /// writer peer first, so the very first operation carries it); the one
    /// commit that follows is the unit's first change. Not validated here:
    /// a constructor that needs a shape check runs it on the finished header.
    pub(crate) fn fresh(
        unit: A,
        init: impl FnOnce(&LoroDoc) -> Result<(), A::Refusal>,
    ) -> Result<Self, A::Refusal> {
        let doc = LoroDoc::new();
        init(&doc)?;
        doc.commit();
        Ok(Self {
            doc,
            unit,
            compatibility: Compatibility::Writable,
        })
    }

    /// Load durable bytes for a known address.
    pub(crate) fn load(bytes: &[u8], unit: A) -> Result<Self, A::Refusal> {
        Self::load_with(bytes, |_| Ok(unit))
    }

    /// Load durable bytes, letting the caller derive the address from the
    /// decoded header before it is validated against that same header.
    pub(crate) fn load_with(
        bytes: &[u8],
        resolve: impl FnOnce(&LoroDoc) -> Result<A, A::Refusal>,
    ) -> Result<Self, A::Refusal> {
        let doc = LoroDoc::new();
        let status = doc
            .import(bytes)
            .map_err(|_| UnitFailure::UndecodableSnapshot)?;
        if status.pending.is_some() {
            return Err(UnitFailure::SnapshotMissingAncestors.into());
        }
        let unit = resolve(&doc)?;
        let compatibility = unit.validate(&doc)?;
        Ok(Self {
            doc,
            unit,
            compatibility,
        })
    }

    pub(crate) fn require_writable(&self) -> Result<(), A::Refusal> {
        match self.compatibility {
            Compatibility::Writable => Ok(()),
            Compatibility::TooNew { found, understood } => {
                Err(UnitFailure::ReadOnly { found, understood }.into())
            }
        }
    }

    pub(crate) fn snapshot(&self) -> Result<Vec<u8>, A::Refusal> {
        self.doc
            .export(ExportMode::snapshot())
            .map_err(|_| UnitFailure::Crdt.into())
    }

    pub(crate) fn updates_since(&self, from: &VersionVector) -> Result<Vec<u8>, A::Refusal> {
        self.require_writable()?;
        self.doc
            .export(ExportMode::updates(from))
            .map_err(|_| UnitFailure::Crdt.into())
    }

    pub(crate) fn version(&self) -> VersionVector {
        self.doc.state_vv()
    }

    /// Fork the held unit, apply every frame to the fork, validate the
    /// complete candidate and return the fork. The held unit is never
    /// touched: a rejected frame cannot poison it, and a pending one leaves
    /// it exactly as it was.
    pub(crate) fn staged_import(&self, frames: &[Vec<u8>]) -> Result<Staged<Self>, A::Refusal> {
        self.require_writable()?;
        let doc = LoroDoc::new();
        doc.import(&self.snapshot()?)
            .map_err(|_| UnitFailure::Crdt)?;
        doc.set_peer_id(self.doc.peer_id())
            .map_err(|_| UnitFailure::Crdt)?;
        for frame in frames {
            // A complete foreign snapshot can lose the metadata LWW conflict
            // while still adding its content. Validate its own scope before
            // merging; a causal delta is checked against the staged state.
            let incoming = LoroDoc::new();
            let isolated = incoming
                .import(frame)
                .map_err(|_| UnitFailure::UndecodableFrame)?;
            if isolated.pending.is_none()
                && integer(&incoming.get_map("meta"), "schema_version").is_some()
            {
                self.unit.validate(&incoming)?;
            }
            let status = doc
                .import(frame)
                .map_err(|_| UnitFailure::UndecodableFrame)?;
            if status.pending.is_some() {
                return Ok(Staged::Pending);
            }
            match self.unit.validate(&doc)? {
                Compatibility::Writable => {}
                Compatibility::TooNew { found, understood } => {
                    return Err(UnitFailure::IncomingTooNew { found, understood }.into());
                }
            }
            self.unit.merged(&self.doc, &doc);
        }
        Ok(Staged::Applied(Self {
            doc,
            unit: self.unit,
            compatibility: self.compatibility,
        }))
    }
}

// ------------------------------------------------------- map value helpers

pub(crate) fn string(map: &loro::LoroMap, key: &str) -> Option<String> {
    match map.get(key)? {
        ValueOrContainer::Value(LoroValue::String(v)) => Some(v.to_string()),
        _ => None,
    }
}

pub(crate) fn integer(map: &loro::LoroMap, key: &str) -> Option<i64> {
    match map.get(key)? {
        ValueOrContainer::Value(LoroValue::I64(v)) => Some(v),
        _ => None,
    }
}

/// Every `(key, value)` of a map, in Loro's iteration order.
pub(crate) fn entries(map: &loro::LoroMap) -> Vec<(String, ValueOrContainer)> {
    let mut out = Vec::new();
    map.for_each(|key, value| out.push((key.to_string(), value)));
    out
}

// ------------------------------------------------- typed JSON history helpers
//
// Shared by every walker of Loro's typed JSON history (the personal
// migration's namespace transcoder and the root-catalog export redaction),
// so each refuses the same shapes by the same names.

/// The kind of an operation, for a refusal message. A unit built of maps
/// holds only `map` operations; every other name here is a refusal.
pub(crate) fn op_kind(content: &JsonOpContent) -> &'static str {
    match content {
        JsonOpContent::Map(_) => "map",
        JsonOpContent::List(_) => "list",
        JsonOpContent::MovableList(_) => "movable list",
        JsonOpContent::Text(_) => "text",
        JsonOpContent::Tree(_) => "tree",
        JsonOpContent::Future(_) => "future",
    }
}

/// The nearest non-mergeable ancestor of a container: a named root for a
/// root or for a chain of `ensure_mergeable_map` children under one, or the
/// ordinary (op-id) container the chain hangs from. A mergeable id encodes
/// its parent and key, so this needs no document.
pub(crate) fn non_mergeable_ancestor(container: &ContainerID) -> ContainerID {
    let mut current = container.clone();
    while let Some((parent, _, _)) = current.parse_mergeable() {
        current = parent;
    }
    current
}
