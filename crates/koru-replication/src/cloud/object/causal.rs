// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_store_core::Manifest;
use koru_store_core::profile_units::{IdentityManifest, PersonalManifest, PrivateManifest};
use loro::{EncodedBlobMode, ExportMode, Frontiers, LoroDoc, VersionVector};
use std::collections::HashSet;
mod stage;
pub use stage::{StagedObject, stage_initial_checkpoint, starts_from_empty};

/// A caller-supplied fact to correlate, never inferred from empty payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckpointOrigin {
    FirstCloudCheckpoint,
    LaterCheckpoint,
}

/// Exact stored head commitment, not a freshly re-encoded semantic VV hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeadCommitment {
    pub result_version_digest: Hash,
}

/// Complete content at one expected head. No live authority, durable receipt,
/// mutable Loro handle, or filesystem object is conferred by this value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContentProof {
    dataset: CloudReplicaPrincipalV1,
    unit: Unit,
    cloud_unit_id: CloudId,
    head: HeadCommitment,
    head_object_sha256: Hash,
    checkpoint_object_sequence: u64,
    last_replayed_segment_sequence: Option<u64>,
}
impl ContentProof {
    pub fn dataset(self) -> CloudReplicaPrincipalV1 {
        self.dataset
    }
    pub fn unit(self) -> Unit {
        self.unit
    }
    pub fn cloud_unit_id(self) -> CloudId {
        self.cloud_unit_id
    }
    pub fn head(self) -> HeadCommitment {
        self.head
    }
    /// Binds this proof to the exact final envelope, not merely an equal VV.
    pub fn head_object_sha256(self) -> Hash {
        self.head_object_sha256
    }
    /// Observed allocation, not the catalog's committed-sequence high-water.
    pub fn checkpoint_object_sequence(self) -> u64 {
        self.checkpoint_object_sequence
    }
    /// Last replayed segment allocation, independently of checkpoint allocation.
    /// Neither this nor their maximum proves the catalog's high-water: recovery
    /// can preserve an allocation for a corrupt object absent from this chain.
    pub fn last_replayed_segment_sequence(self) -> Option<u64> {
        self.last_replayed_segment_sequence
    }
}

/// The fresh, fully verified content at one exact captured head. It grants no
/// authorization or SQL receipt. No mutable reference to the retained document
/// escapes; exports run on the owning worker thread.
pub struct RestoredContent {
    doc: LoroDoc,
    proof: ContentProof,
    result_version: Vec<u8>,
    limits: Limits,
    object_ids: HashSet<CloudId>,
}
impl RestoredContent {
    pub fn proof(&self) -> ContentProof {
        self.proof
    }
    /// Preserve the exact captured bytes: equivalent VV encodings can differ.
    pub fn result_version(&self) -> &[u8] {
        &self.result_version
    }
    pub fn snapshot(&self) -> Result<Vec<u8>, Error> {
        let bytes = self
            .doc
            .export(ExportMode::snapshot())
            .map_err(|_| Error::Loro)?;
        self.export_payload(Kind::Checkpoint, bytes.len())?;
        Ok(bytes)
    }
    pub fn updates_since(&self, version: &[u8]) -> Result<Vec<u8>, Error> {
        if version.len() > self.limits.vector as usize {
            tracing::warn!(
                dataset = ?self.proof.dataset,
                unit = ?self.proof.unit,
                direction = "download",
                check = "worker_export_version",
                actual_bytes = version.len(),
                limit_bytes = self.limits.vector,
                "cloud sync: size limit rejected requested version metadata"
            );
            return Err(Error::TooLarge);
        }
        let version = VersionVector::decode(version).map_err(|_| Error::Loro)?;
        let bytes = self
            .doc
            .export(ExportMode::updates(&version))
            .map_err(|_| Error::Loro)?;
        self.export_payload(Kind::Segment, bytes.len())?;
        Ok(bytes)
    }

    fn export_payload(&self, kind: Kind, bytes: usize) -> Result<(), Error> {
        self.limits.payload(kind, bytes as u64).inspect_err(|_| {
            tracing::warn!(
                dataset = ?self.proof.dataset,
                unit = ?self.proof.unit,
                direction = "download",
                check = "worker_export_payload",
                export_kind = ?kind,
                actual_bytes = bytes,
                limit_bytes = match kind {
                    Kind::Checkpoint => self.limits.checkpoint,
                    Kind::Segment => self.limits.segment,
                },
                "cloud sync: size limit rejected export"
            );
        })
    }
}
impl std::fmt::Debug for RestoredContent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RestoredContent")
            .field("proof", &self.proof)
            .field("result_version_bytes", &self.result_version.len())
            .finish_non_exhaustive()
    }
}

enum ProfileModel {
    Identity(IdentityManifest),
    Private(PrivateManifest),
    Personal(PersonalManifest),
}
impl ProfileModel {
    fn load(object: BoundObject<'_>) -> Result<Option<Self>, Error> {
        Self::from_snapshot(object.metadata(), object.body().payload)
    }
    fn from_snapshot(metadata: Metadata, payload: &[u8]) -> Result<Option<Self>, Error> {
        let model = match metadata.unit {
            Unit::ProfileIdentity => {
                Self::Identity(IdentityManifest::from_snapshot(payload).map_err(|_| Error::Scope)?)
            }
            Unit::ProfilePrivate => {
                Self::Private(PrivateManifest::from_snapshot(payload).map_err(|_| Error::Scope)?)
            }
            Unit::ProfilePersonal { .. } => {
                Self::Personal(PersonalManifest::from_snapshot(payload).map_err(|_| Error::Scope)?)
            }
            _ => return Ok(None),
        };
        model.check(metadata)?;
        Ok(Some(model))
    }
    fn check(&self, metadata: Metadata) -> Result<(), Error> {
        let (unit, writable) = match self {
            Self::Identity(m) => (m.unit(), m.compatibility().writable()),
            Self::Private(m) => (m.unit(), m.compatibility().writable()),
            // A v2 personal unit has no v1 address; this path carries v1 only.
            Self::Personal(m) => (m.unit().ok_or(Error::Scope)?, m.compatibility().writable()),
        };
        if unit != metadata.unit.profile_unit(metadata.dataset)? {
            return Err(Error::Scope);
        }
        if !writable {
            return Err(Error::Schema);
        }
        Ok(())
    }
    fn stage(&self, object: BoundObject<'_>) -> Result<Self, Error> {
        self.stage_payload(object.metadata(), object.body().payload)
    }
    fn stage_payload(&self, metadata: Metadata, payload: &[u8]) -> Result<Self, Error> {
        // Reuse the existing validator, including isolated incoming snapshot
        // scope checks before metadata LWW could hide a foreign scope. This API
        // takes owned frames; the copy is bounded, not a claim about Loro memory.
        let frame = payload.to_vec();
        let model = match self {
            Self::Identity(m) => {
                Self::Identity(m.staged_import(&[frame]).map_err(|_| Error::Scope)?)
            }
            Self::Private(m) => Self::Private(m.staged_import(&[frame]).map_err(|_| Error::Scope)?),
            Self::Personal(m) => {
                Self::Personal(m.staged_import(&[frame]).map_err(|_| Error::Scope)?)
            }
        };
        model.check(metadata)?;
        Ok(model)
    }
}

fn workspace_model(doc: &LoroDoc, metadata: Metadata) -> Result<(), Error> {
    match metadata.unit {
        Unit::WorkspaceManifest => {
            // No adopt API exists for Manifest. Exporting this private fresh
            // reconstruction for its established reader never replaces payload.
            let snapshot = doc
                .export(ExportMode::snapshot())
                .map_err(|_| Error::Loro)?;
            let model = Manifest::from_snapshot(&snapshot).map_err(|_| Error::Schema)?;
            let (_, authority, _) = identity::parts(metadata.dataset);
            if model.workspace_id().map(|id| id.to_storage())
                != Some(identity::uuid_text(authority))
            {
                return Err(Error::Scope);
            }
            if !model.compatibility().writable() {
                return Err(Error::Schema);
            }
        }
        Unit::WorkspaceDocument { .. } => {
            let model =
                koru_core::schema::WorkspaceDoc::adopt(doc.fork()).map_err(|_| Error::Schema)?;
            if !model.compatibility().writable() {
                return Err(Error::Schema);
            }
            // Store identity is external. Missing metadata is valid legacy v1;
            // id() names a workspace and own_document_id() can be a node fallback.
            // Never stamp or rewrite a captured object while verifying it.
        }
        _ => {}
    }
    Ok(())
}

fn applied(doc: &LoroDoc, payload: &[u8]) -> Result<(), Error> {
    let status = doc.import(payload).map_err(|_| Error::Loro)?;
    if status.pending.is_some() {
        Err(Error::Ancestors)
    } else {
        Ok(())
    }
}
fn result(doc: &LoroDoc, bytes: &[u8]) -> Result<(), Error> {
    let expected = VersionVector::decode(bytes).map_err(|_| Error::Loro)?;
    if doc.state_vv() != expected {
        Err(Error::Result)
    } else {
        Ok(())
    }
}
fn limits_for(object: BoundObject<'_>, limits: Limits) -> Result<(), Error> {
    let c = object.commitments();
    let header = c
        .object_byte_length
        .checked_sub(c.payload_byte_length)
        .and_then(|n| n.checked_sub(PREFIX_BYTES as u64))
        .ok_or(Error::Integer)?;
    preflight_length(
        u32::try_from(header).map_err(|_| Error::TooLarge)?,
        c.payload_byte_length,
        limits,
    )?;
    limits.payload(object.metadata().kind, c.payload_byte_length)?;
    for (field, bytes) in [
        ("predecessor_version", object.body().predecessor_version),
        ("result_version", object.body().result_version),
        ("checkpoint_frontiers", object.body().checkpoint_frontiers),
    ] {
        if bytes.len() as u64 > u64::from(limits.vector) {
            tracing::warn!(
                dataset = ?object.metadata().dataset,
                unit = ?object.metadata().unit,
                check = "object_restore_vector",
                field,
                actual_bytes = bytes.len(),
                limit_bytes = limits.vector,
                "cloud object: size limit rejected restored version metadata"
            );
            return Err(Error::TooLarge);
        }
    }
    Ok(())
}

/// Fresh checkpoint + exact receipted predecessor chain, never recovery to an
/// older/empty substitute. Sequence gaps are legal (abandoned reservations),
/// but segment order after checkpoint coverage must strictly advance and each captured predecessor must
/// equal the exact preceding result bytes. Serialized bounds do not prove
/// replay cost or expanded-memory capacity; this is a disconnected candidate.
pub fn verify_chain(
    checkpoint: BoundObject<'_>,
    origin: CheckpointOrigin,
    segments: &[BoundObject<'_>],
    expected_head: HeadCommitment,
    limits: Limits,
) -> Result<ContentProof, Error> {
    reconstruct(checkpoint, origin, segments, expected_head, limits).map(|(proof, _, _)| proof)
}

/// Retains the SAME reconstruction used by verify_chain, plus its original
/// head-version bytes. Incomplete/mismatched chains return no document at all.
pub fn restore_chain(
    checkpoint: BoundObject<'_>,
    origin: CheckpointOrigin,
    segments: &[BoundObject<'_>],
    expected_head: HeadCommitment,
    limits: Limits,
) -> Result<RestoredContent, Error> {
    let (proof, doc, object_ids) =
        reconstruct(checkpoint, origin, segments, expected_head, limits)?;
    let head = segments.last().copied().unwrap_or(checkpoint);
    let mut result_version = Vec::new();
    result_version
        .try_reserve_exact(head.body().result_version.len())
        .map_err(|_| Error::Allocation)?;
    result_version.extend_from_slice(head.body().result_version);
    Ok(RestoredContent {
        doc,
        proof,
        result_version,
        limits,
        object_ids,
    })
}

fn reconstruct(
    checkpoint: BoundObject<'_>,
    origin: CheckpointOrigin,
    segments: &[BoundObject<'_>],
    expected_head: HeadCommitment,
    limits: Limits,
) -> Result<(ContentProof, LoroDoc, HashSet<CloudId>), Error> {
    let base = checkpoint.metadata();
    if base.kind != Kind::Checkpoint
        || (base.covers_through_sequence == 0) != (origin == CheckpointOrigin::FirstCloudCheckpoint)
    {
        return Err(Error::Chain);
    }
    // Validate all declarations/correlations before the first Loro import.
    limits_for(checkpoint, limits)?;
    let mut prior = checkpoint;
    let mut last_sequence = base.covers_through_sequence;
    let mut object_ids = HashSet::new();
    object_ids
        .try_reserve(segments.len().checked_add(1).ok_or(Error::Integer)?)
        .map_err(|_| Error::Allocation)?;
    object_ids.insert(base.object_id);
    for &segment in segments {
        limits_for(segment, limits)?;
        let metadata = segment.metadata();
        if metadata.kind != Kind::Segment
            || metadata.dataset != base.dataset
            || metadata.unit != base.unit
            || metadata.cloud_unit_id != base.cloud_unit_id
            || metadata.sequence <= last_sequence
            || metadata.sequence == base.sequence
            || !object_ids.insert(metadata.object_id)
        {
            return Err(Error::Chain);
        }
        if segment.body().predecessor_version != prior.body().result_version {
            return Err(Error::Predecessor);
        }
        prior = segment;
        last_sequence = metadata.sequence;
    }
    let head = HeadCommitment {
        result_version_digest: prior.commitments().result_version_digest,
    };
    if head != expected_head {
        return Err(Error::Chain);
    }
    // A self-contained update or shallow snapshot can import to the advertised
    // state too. The signed checkpoint format requires history-retaining Snapshot.
    let checkpoint_meta = LoroDoc::decode_import_blob_meta(checkpoint.body().payload, true)
        .map_err(|_| Error::Loro)?;
    if checkpoint_meta.mode != EncodedBlobMode::Snapshot {
        return Err(Error::CausalShape);
    }
    let doc = LoroDoc::new();
    applied(&doc, checkpoint.body().payload)?;
    result(&doc, checkpoint.body().result_version)?;
    let frontiers =
        Frontiers::decode(checkpoint.body().checkpoint_frontiers).map_err(|_| Error::Loro)?;
    if doc.state_frontiers() != frontiers {
        return Err(Error::Frontier);
    }
    let mut profile = ProfileModel::load(checkpoint)?;
    workspace_model(&doc, base)?;
    for &segment in segments {
        let before =
            VersionVector::decode(segment.body().predecessor_version).map_err(|_| Error::Loro)?;
        if doc.state_vv() != before {
            return Err(Error::Predecessor);
        }
        let meta = LoroDoc::decode_import_blob_meta(segment.body().payload, true)
            .map_err(|_| Error::Loro)?;
        if let Some(model) = profile.as_ref() {
            profile = Some(model.stage(segment)?);
        }
        applied(&doc, segment.body().payload)?;
        if !doc.state_vv().includes_vv(&meta.partial_end_vv) {
            return Err(Error::Ancestors);
        }
        result(&doc, segment.body().result_version)?;
        workspace_model(&doc, segment.metadata())?;
    }
    Ok((
        ContentProof {
            dataset: base.dataset,
            unit: base.unit,
            cloud_unit_id: base.cloud_unit_id,
            head,
            head_object_sha256: prior.commitments().object_sha256,
            checkpoint_object_sequence: base.sequence,
            last_replayed_segment_sequence: segments.last().map(|s| s.metadata().sequence),
        },
        doc,
        object_ids,
    ))
}

#[cfg(test)]
mod tests;
