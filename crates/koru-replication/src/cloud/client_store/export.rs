// SPDX-License-Identifier: MPL-2.0

//! Local reconciliation reads run under the same serialized Store ownership as
//! receive. No progress database, transport authority or inventory is invented.
use super::*;
use crate::cloud::transfer::{MemoryBudget, Reservation, UploadContext};
use loro::VersionVector;

/// A captured durable local version and its missing bytes. This is NOT evidence
/// of remote coverage. Retain the entire value until upload completes/discards.
pub struct Export {
    unit: Unit,
    version: VersionVector,
    bytes: Vec<u8>,
    upload: UploadContext,
    _reservation: Reservation,
    _excess_capacity: Option<Reservation>,
    _context: Option<Box<dyn Send + Sync>>,
}
impl Export {
    pub fn unit(&self) -> Unit {
        self.unit
    }
    pub fn version(&self) -> &VersionVector {
        &self.version
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    /// What the Store says about this upload besides its bytes (TECHNICAL
    /// §5.4 item 4). A device's Store never sets it; a hosted replica's
    /// Store port sets the claim reference of the call whose effect this
    /// export carries, with [`Export::with_upload`].
    pub fn upload_context(&self) -> UploadContext {
        self.upload
    }
    /// Attach the upload's context. The Reconciler hands it to the carrier,
    /// which writes `claim` into the upload's `TransferStart`; a session that
    /// may not carry one refuses the upload as `sync_transfer_invalid`.
    pub fn with_upload(mut self, context: UploadContext) -> Self {
        self.upload = context;
        self
    }
    /// Move the payload into Client::upload and pass the second value as its
    /// retained guard. Moving bytes must not release their export reservation.
    pub fn into_payload(mut self) -> (Vec<u8>, Self) {
        (std::mem::take(&mut self.bytes), self)
    }
}

/// Reserve the maximum transferable payload BEFORE queue admission/export.
/// Queue rejection or cancellation before admission drops this reservation.
/// After admission the Store job owns it independently of reply interest.
///
/// Pinned Loro exports into Vec, not a bounded writer. This reservation bounds
/// admitted retained payloads, NOT Loro's temporary heap/oversized export peak.
/// Capacity measurement remains required before activating this composition.
pub struct ExportRequest {
    scope: Scope,
    unit: Unit,
    known: VersionVector,
    budget: MemoryBudget,
    reservation: Reservation,
    context: Option<Box<dyn Send + Sync>>,
}
impl ExportRequest {
    pub fn new(
        scope: Scope,
        unit: Unit,
        known: VersionVector,
        budget: MemoryBudget,
    ) -> Result<Self, Error> {
        scope.object_unit(unit)?;
        let reservation = budget.reserve(transfer::MAX_PAYLOAD)?;
        Ok(Self {
            scope,
            unit,
            known,
            budget,
            reservation,
            context: None,
        })
    }

    /// The unit this request would export.
    pub fn unit(&self) -> Unit {
        self.unit
    }

    /// Keep the metadata/authority resource which supplied this request alive
    /// inside accepted Store work, even when its external waiter is canceled.
    /// This resource grants no authority; execution still rechecks authorization.
    pub fn retain<T: Send + Sync + 'static>(mut self, context: T) -> Self {
        self.context = Some(Box::new(context));
        self
    }

    /// Current authorization runs at execution, before loading/committing data.
    /// An empty remote VV requires a snapshot. Equal/ahead remote VVs require
    /// no transfer; Loro's nonempty encoding of an empty delta is not an edit.
    pub fn run<C: Catalog, B: Backend>(
        self,
        store: &mut Store<C, B>,
        at: Timestamp,
        authorize: impl FnOnce(&mut Store<C, B>, Scope, Unit) -> Result<(), Refusal>,
    ) -> Result<Option<Export>, Failure> {
        let target = authorized_target(store, self.scope, self.unit, authorize)?;
        let version = target.version(store, at)?;
        if !crate::reconcile::missing(&version, &self.known).upload {
            return Ok(None);
        }
        let bytes = match target {
            Target::Document(workspace, document) if self.known.is_empty() => {
                store.snapshot(workspace, document)?
            }
            Target::Document(workspace, document) => {
                store.updates_since(workspace, document, &self.known)?
            }
            Target::Profile(unit) if self.known.is_empty() => store.profile_unit_snapshot(unit)?,
            Target::Profile(unit) => store.profile_unit_updates_since(unit, &self.known)?,
        };
        if bytes.len() > transfer::MAX_PAYLOAD {
            // Sizes and typed IDs only: never log exports or version contents.
            tracing::warn!(
                unit = ?self.unit,
                direction = "upload",
                check = "client_export_payload",
                export_kind = if self.known.is_empty() { "snapshot" } else { "updates" },
                actual_bytes = bytes.len(),
                limit_bytes = transfer::MAX_PAYLOAD,
                "cloud sync: size limit rejected export"
            );
            return Err(Error::TooLarge.into());
        }
        if bytes.is_empty() {
            return Err(Error::Invalid.into());
        }
        // Vec's retained capacity can exceed its encoded length. Never hand an
        // uncharged excess allocation across the Store reply boundary.
        let excess = bytes.capacity().saturating_sub(transfer::MAX_PAYLOAD);
        let excess_capacity = (excess != 0)
            .then(|| self.budget.reserve(excess))
            .transpose()?;
        Ok(Some(Export {
            unit: self.unit,
            version,
            bytes,
            upload: UploadContext::default(),
            _reservation: self.reservation,
            _excess_capacity: excess_capacity,
            _context: self.context,
        }))
    }
}

/// Read the version advertised by a join, not an unflushed in-memory position.
/// Workspace flush uses its ordinary commit/event path; Profile units are
/// already atomically committed. A missing Profile unit has the empty vector,
/// but no placeholder bytes are created and a removed Profile stays an error.
pub fn version<C: Catalog, B: Backend>(
    store: &mut Store<C, B>,
    scope: Scope,
    unit: Unit,
    at: Timestamp,
    authorize: impl FnOnce(&mut Store<C, B>, Scope, Unit) -> Result<(), Refusal>,
) -> Result<VersionVector, Failure> {
    authorized_target(store, scope, unit, authorize)?.version(store, at)
}

enum Target {
    Document(WorkspaceId, DocumentId),
    Profile(ProfileUnitRef),
}
impl Target {
    fn version<C: Catalog, B: Backend>(
        &self,
        store: &mut Store<C, B>,
        at: Timestamp,
    ) -> Result<VersionVector, Failure> {
        Ok(match *self {
            Self::Document(workspace, document) => {
                // version opens the exact existing document before flush.
                // Nothing else can mutate this Store between these operations.
                let version = store.version(workspace, document)?;
                store.flush(workspace, document, at)?;
                version
            }
            Self::Profile(unit) => store.profile_unit_version(unit)?,
        })
    }
}

fn authorized_target<C: Catalog, B: Backend>(
    store: &mut Store<C, B>,
    scope: Scope,
    unit: Unit,
    authorize: impl FnOnce(&mut Store<C, B>, Scope, Unit) -> Result<(), Refusal>,
) -> Result<Target, Failure> {
    scope.object_unit(unit)?;
    authorize(store, scope, unit).map_err(Error::Authority)?;
    Ok(match unit {
        Unit::Document {
            workspace_id,
            document_id,
        } => {
            let workspace = WorkspaceId::parse(&workspace_id.to_string()).ok_or(Error::Invalid)?;
            let document = DocumentId::parse(&document_id.to_string()).ok_or(Error::Invalid)?;
            if store.manifest_id(workspace)? != scope.manifest {
                return Err(Error::Invalid.into());
            }
            Target::Document(workspace, document)
        }
        Unit::Identity { profile_id } => Target::Profile(ProfileUnitRef::Identity {
            profile: ProfileId::parse(&profile_id.to_string()).ok_or(Error::Invalid)?,
        }),
        Unit::Private { profile_id } => Target::Profile(ProfileUnitRef::Private {
            profile: ProfileId::parse(&profile_id.to_string()).ok_or(Error::Invalid)?,
        }),
        Unit::Personal {
            profile_id,
            workspace_id,
        } => Target::Profile(ProfileUnitRef::Personal {
            owner: ProfileId::parse(&profile_id.to_string()).ok_or(Error::Invalid)?,
            workspace: WorkspaceId::parse(&workspace_id.to_string()).ok_or(Error::Invalid)?,
        }),
    })
}
