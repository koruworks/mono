// SPDX-License-Identifier: MPL-2.0

//! The two unit families as documents, on the shared staging mechanism.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use loro::{LoroDoc, VersionVector};
use uuid::Uuid;

use super::lifecycle::{DeletionRecord, Facts, Placement, PurgeRecord, RestoreRecord};
use super::{
    CatalogUnitRef, CatalogUnitRefusal as Refusal, K_PROFILE_ID, K_SCHEMA_VERSION, K_UNIT_KIND,
    K_WORKSPACE_ID, PlacementConcurrency, R_META, Staged, validation,
};
use crate::ids::{DeletionId, ProfileId, RestoreId, WorkspaceId};
use crate::unit_doc::{UnitAddress, UnitDoc};
use crate::version::{CATALOG_UNIT_FORMAT_VERSION, Compatibility};

impl UnitAddress for CatalogUnitRef {
    type Refusal = Refusal;

    fn validate(&self, doc: &LoroDoc) -> Result<Compatibility, Refusal> {
        validation::validate(doc, *self)
    }
}

/// Author a fresh lineage whose first operation already carries the writer.
/// Each unit has its own persisted writer identity; a Store-global stream
/// would collide the moment two independently created units met.
fn fresh(unit: CatalogUnitRef, writer_peer: u64) -> Result<UnitDoc<CatalogUnitRef>, Refusal> {
    UnitDoc::fresh(unit, |doc| {
        doc.set_peer_id(writer_peer).map_err(|_| Refusal::Crdt)?;
        let meta = doc.get_map(R_META);
        meta.insert(K_SCHEMA_VERSION, CATALOG_UNIT_FORMAT_VERSION)
            .map_err(|_| Refusal::Crdt)?;
        meta.insert(K_UNIT_KIND, unit.kind().as_str())
            .map_err(|_| Refusal::Crdt)?;
        let key = match unit {
            CatalogUnitRef::Profile { .. } => K_PROFILE_ID,
            CatalogUnitRef::Workspace { .. } => K_WORKSPACE_ID,
        };
        meta.insert(key, unit.subject_id())
            .map_err(|_| Refusal::Crdt)?;
        Ok(())
    })
}

/// Load committed bytes for a known address and hand the writer stream to
/// `writer_peer`. A read-only unit gets no writer: nothing will write it.
fn load(
    bytes: &[u8],
    unit: CatalogUnitRef,
    writer_peer: u64,
) -> Result<UnitDoc<CatalogUnitRef>, Refusal> {
    let inner = UnitDoc::load(bytes, unit)?;
    if inner.compatibility.writable() {
        inner
            .doc
            .set_peer_id(writer_peer)
            .map_err(|_| Refusal::Crdt)?;
    }
    Ok(inner)
}

/// A first arrival: frames for a unit this device does not hold yet. They
/// are imported into an empty document — not merged into a fresh header, so
/// this replica authors nothing into a history it only received — and
/// handed back as the complete snapshot the address constructor then
/// validates (header, kind, subject, placement). A frame whose ancestors
/// are missing is [`Staged::Pending`]: the sender needs the empty version.
fn fresh_from_frames(frames: &[Vec<u8>]) -> Result<Staged<Vec<u8>>, Refusal> {
    let doc = LoroDoc::new();
    for frame in frames {
        let status = doc.import(frame).map_err(|_| Refusal::UndecodableFrame)?;
        if status.pending.is_some() {
            return Ok(Staged::Pending);
        }
    }
    Ok(Staged::Applied(
        doc.export(loro::ExportMode::snapshot())
            .map_err(|_| Refusal::Crdt)?,
    ))
}

// Both families share the staging boundary, the lifecycle facts and the
// reducers. Only the Workspace unit has a placement.
macro_rules! catalog_unit_methods {
    () => {
        pub fn unit(&self) -> CatalogUnitRef {
            self.inner.unit
        }
        pub fn compatibility(&self) -> Compatibility {
            self.inner.compatibility
        }
        pub fn is_read_only(&self) -> bool {
            !self.compatibility().writable()
        }
        /// The peer id every new operation of this replica is written under.
        pub fn writer_peer(&self) -> u64 {
            self.inner.doc.peer_id()
        }
        /// The complete durable bytes for a `root_catalog_units` row. Refused
        /// when the row contract would refuse them on the way back in — a
        /// live Workspace unit with no placement is the one way a correctly
        /// authored unit can get here.
        pub fn snapshot(&self) -> Result<Vec<u8>, Refusal> {
            validation::validate(&self.inner.doc, self.inner.unit)?;
            self.inner.snapshot()
        }
        pub fn updates_since(&self, from: &VersionVector) -> Result<Vec<u8>, Refusal> {
            self.inner.updates_since(from)
        }
        pub fn version(&self) -> VersionVector {
            self.inner.version()
        }
        /// Apply frames to a disposable fork and validate the complete
        /// candidate; this unit is untouched. [`Staged::Pending`] means a
        /// frame needed ancestors this unit does not have: nothing was
        /// applied, and the sender needs [`Self::version`] to fill the gap.
        pub fn staged_import(&self, frames: &[Vec<u8>]) -> Result<Staged<Self>, Refusal> {
            Ok(self
                .inner
                .staged_import(frames)?
                .map(|inner| Self { inner }))
        }
        /// [`Self::staged_import`], then adopt the candidate when it applied.
        pub fn import_frames(&mut self, frames: &[Vec<u8>]) -> Result<Staged<()>, Refusal> {
            match self.staged_import(frames)? {
                Staged::Applied(next) => {
                    *self = next;
                    Ok(Staged::Applied(()))
                }
                Staged::Pending => Ok(Staged::Pending),
            }
        }

        fn facts(&self) -> Facts<'_> {
            Facts {
                doc: &self.inner.doc,
            }
        }
        pub fn deletions(&self) -> BTreeMap<DeletionId, DeletionRecord> {
            self.facts().deletions()
        }
        pub fn restores(&self) -> BTreeMap<RestoreId, RestoreRecord> {
            self.facts().restores()
        }
        pub fn purged(&self) -> BTreeMap<DeletionId, PurgeRecord> {
            self.facts().purged()
        }
        /// Deletion ids not named by any restore's `observed_deletions`.
        pub fn active_deletions(&self) -> BTreeSet<DeletionId> {
            self.facts().active_deletions()
        }
        pub fn is_deleted(&self) -> bool {
            self.facts().is_deleted()
        }
        /// Neither deleted nor purged.
        pub fn is_live(&self) -> bool {
            self.facts().is_live()
        }
        /// Append one deletion fact. An existing id is refused, never rewritten.
        pub fn record_deletion(
            &self,
            id: DeletionId,
            record: &DeletionRecord,
        ) -> Result<(), Refusal> {
            self.inner.require_writable()?;
            self.facts().record_deletion(id, record)
        }
        /// Append one restore fact naming the deletions it observed.
        pub fn record_restore(&self, id: RestoreId, record: &RestoreRecord) -> Result<(), Refusal> {
            self.inner.require_writable()?;
            self.facts().record_restore(id, record)
        }
        /// Append the terminal purge evidence for one deletion.
        pub fn record_purge(&self, id: DeletionId, record: &PurgeRecord) -> Result<(), Refusal> {
            self.inner.require_writable()?;
            self.facts().record_purge(id, record)
        }
    };
}

/// One Profile's existence and shared lifecycle. It holds no list of the
/// Profile's Workspaces: a cascade's targets stay in the local job, and each
/// Workspace unit carries its own deletion fact with the batch id.
pub struct ProfileCatalogUnit {
    pub(super) inner: UnitDoc<CatalogUnitRef>,
}

impl fmt::Debug for ProfileCatalogUnit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProfileCatalogUnit")
            .field("unit", &self.inner.unit)
            .field("version", &self.version())
            .finish()
    }
}

impl ProfileCatalogUnit {
    pub fn create(profile: ProfileId, writer_peer: u64) -> Result<Self, Refusal> {
        Ok(Self {
            inner: fresh(CatalogUnitRef::Profile { profile }, writer_peer)?,
        })
    }

    pub fn from_snapshot(
        bytes: &[u8],
        profile: ProfileId,
        writer_peer: u64,
    ) -> Result<Self, Refusal> {
        Ok(Self {
            inner: load(bytes, CatalogUnitRef::Profile { profile }, writer_peer)?,
        })
    }

    pub fn profile(&self) -> ProfileId {
        self.inner
            .unit
            .profile()
            .expect("a Profile catalog unit is addressed by a Profile")
    }

    /// A unit this device did not hold, from a peer's frames (see
    /// [`fresh_from_frames`]); validated as `profile`'s unit before it is
    /// returned, so a frame whose header names another subject is refused.
    pub fn from_frames(
        frames: &[Vec<u8>],
        profile: ProfileId,
        writer_peer: u64,
    ) -> Result<Staged<Self>, Refusal> {
        match fresh_from_frames(frames)? {
            Staged::Pending => Ok(Staged::Pending),
            Staged::Applied(bytes) => Ok(Staged::Applied(Self::from_snapshot(
                &bytes,
                profile,
                writer_peer,
            )?)),
        }
    }

    catalog_unit_methods!();
}

/// One Workspace's placement and lifecycle for this user. Name, icon and
/// color stay in the Workspace manifest; nothing here describes the content.
pub struct WorkspaceCatalogUnit {
    pub(super) inner: UnitDoc<CatalogUnitRef>,
}

impl fmt::Debug for WorkspaceCatalogUnit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkspaceCatalogUnit")
            .field("unit", &self.inner.unit)
            .field("placement", &self.placement())
            .field("version", &self.version())
            .finish()
    }
}

impl WorkspaceCatalogUnit {
    /// A fresh unit with its placement **pending**: [`Self::placement`] is
    /// `None` until [`Self::set_placement`] runs, and [`Self::snapshot`]
    /// refuses until then.
    pub fn create(workspace: WorkspaceId, writer_peer: u64) -> Result<Self, Refusal> {
        Ok(Self {
            inner: fresh(CatalogUnitRef::Workspace { workspace }, writer_peer)?,
        })
    }

    pub fn from_snapshot(
        bytes: &[u8],
        workspace: WorkspaceId,
        writer_peer: u64,
    ) -> Result<Self, Refusal> {
        Ok(Self {
            inner: load(bytes, CatalogUnitRef::Workspace { workspace }, writer_peer)?,
        })
    }

    pub fn workspace(&self) -> WorkspaceId {
        self.inner
            .unit
            .workspace()
            .expect("a Workspace catalog unit is addressed by a Workspace")
    }

    /// A unit this device did not hold, from a peer's frames (see
    /// [`fresh_from_frames`]); validated as `workspace`'s unit — header,
    /// kind, subject and a committed placement — before it is returned.
    pub fn from_frames(
        frames: &[Vec<u8>],
        workspace: WorkspaceId,
        writer_peer: u64,
    ) -> Result<Staged<Self>, Refusal> {
        match fresh_from_frames(frames)? {
            Staged::Pending => Ok(Staged::Pending),
            Staged::Applied(bytes) => Ok(Staged::Applied(Self::from_snapshot(
                &bytes,
                workspace,
                writer_peer,
            )?)),
        }
    }

    /// The current register, or `None` while placement is pending.
    pub fn placement(&self) -> Option<Placement> {
        self.facts().placement()
    }

    /// **Q10 case 3's detection.** The placement writes this unit's history
    /// gained since `before` that are *concurrent* with `operation` — an
    /// operation this device recorded earlier, typically its own bootstrap
    /// repair — each with the placement its own history reads.
    ///
    /// Three answers, not a boolean:
    /// [`PlacementConcurrency::Unknown`] when `operation` is not in this
    /// history at all, which must never read as a conflict;
    /// [`PlacementConcurrency::Ordered`] when every added write either saw
    /// it or is in its past, and whether one of them also saw every rival in
    /// `recorded` and so answers the question already; and
    /// [`PlacementConcurrency::Concurrent`] when one did neither *and*
    /// touched the register — whether or not the merged value moved, because
    /// in the case this is for it does not.
    pub fn placement_writes_concurrent_with(
        &self,
        before: &VersionVector,
        operation: loro::ID,
        recorded: &[loro::ID],
    ) -> PlacementConcurrency {
        super::concurrency::placement_writes_concurrent_with(
            &self.inner.doc,
            before,
            &self.version(),
            operation,
            recorded,
        )
    }

    /// Write the whole `{profile_id, move_id}` record in one commit.
    /// `None` is *explicitly unassigned*. Concurrent calls on two replicas
    /// converge by Loro's map ordering; nothing here consults a clock.
    pub fn set_placement(&self, profile: Option<ProfileId>, move_id: Uuid) -> Result<(), Refusal> {
        self.inner.require_writable()?;
        self.facts().set_placement(&Placement { profile, move_id })
    }

    catalog_unit_methods!();
}
